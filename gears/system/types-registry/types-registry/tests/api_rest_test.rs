#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use toolkit::api::{OpenApiRegistry, ParamLocation, ResponseHeaderType};
use toolkit_db::{DBProvider, DbError};
use toolkit_gts::{gts_id, gts_uri};
use tower::ServiceExt;

use types_registry::api::rest::routes::{V1, V2};
use types_registry::config::TypesRegistryConfig;
use types_registry::domain::admission::OperationDispatch;
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::domain::registry_service::{MAX_BATCH_GET_KEYS, RegistryService};
use types_registry::domain::service::TypesRegistryService;
use types_registry::infra::InMemoryGtsRepository;
use types_registry::infra::outbox::OutboxDispatch;

mod common;
use common::stores;

const CF_TYPE: &str = gts_id!("cf.core.example.type.v1~");
const CF_OTHER: &str = gts_id!("cf.core.example.other.v1~");
const CF_THIRD: &str = gts_id!("cf.core.example.third.v1~");
/// An Instance of [`CF_TYPE`]: a full five-token last segment with no trailing `~`.
const CF_INSTANCE: &str = gts_id!("cf.core.example.type.v1~cf.core.example.first.v1");
const INVALID_ARGUMENT_TYPE: &str =
    gts_uri!("cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~");
const HTTP_REQUEST_RESOURCE_TYPE: &str = gts_id!("cf.core.http.request.v1~");

const MUTATION_OPERATIONS: [&str; 4] = [
    "types_registry.register",
    "types_registry.submit_entities",
    "types_registry.batch_delete_entities",
    "types_registry.delete_entity",
];

const V2_MUTATION_OPERATIONS: [&str; 3] = [
    "types_registry.submit_entities",
    "types_registry.batch_delete_entities",
    "types_registry.delete_entity",
];

type DeclaredParam = (
    String,
    ParamLocation,
    bool,
    String,
    Option<String>,
    Option<f64>,
);

fn declared_format(param: &toolkit::api::ParamSpec) -> Option<String> {
    param.format.clone()
}
/// One response as the generated document will carry it: status and content type.
type DeclaredResponse = (u16, String);
/// One response header: status, name, and JSON Schema scalar type.
type DeclaredResponseHeader = (u16, String, ResponseHeaderType);

/// Records what `register_routes` declares, so the generated document is
/// assertable instead of eyeballed in `/cf/docs`.
#[derive(Default)]
struct TestOpenApi {
    /// `(method, path, operation_id)` in registration order.
    operations: std::sync::Mutex<Vec<(String, String, String)>>,
    params: std::sync::Mutex<Vec<(String, Vec<DeclaredParam>)>>,
    /// `operation_id -> [(status, content type)]`.
    responses: std::sync::Mutex<Vec<(String, Vec<DeclaredResponse>)>>,
    /// `operation_id -> [(status, header name, scalar type)]`.
    response_headers: std::sync::Mutex<Vec<(String, Vec<DeclaredResponseHeader>)>>,
    /// `operation_id -> gateway visibility`.
    exposure: std::sync::Mutex<Vec<(String, bool)>>,
}

impl OpenApiRegistry for TestOpenApi {
    fn register_operation(&self, spec: &toolkit::api::OperationSpec) {
        self.operations.lock().expect("operations lock").push((
            spec.method.to_string(),
            spec.path.clone(),
            spec.operation_id.clone().unwrap_or_default(),
        ));
        self.params.lock().expect("params lock").push((
            spec.operation_id.clone().unwrap_or_default(),
            spec.params
                .iter()
                .map(|p| {
                    (
                        p.name.clone(),
                        p.location.clone(),
                        p.required,
                        p.param_type.clone(),
                        declared_format(p),
                        p.minimum,
                    )
                })
                .collect(),
        ));
        self.responses.lock().expect("responses lock").push((
            spec.operation_id.clone().unwrap_or_default(),
            spec.responses
                .iter()
                .map(|r| (r.status, r.content_type.to_owned()))
                .collect(),
        ));
        self.response_headers
            .lock()
            .expect("response headers lock")
            .push((
                spec.operation_id.clone().unwrap_or_default(),
                spec.responses
                    .iter()
                    .flat_map(|response| {
                        response.headers.iter().map(move |header| {
                            (response.status, header.name.clone(), header.header_type)
                        })
                    })
                    .collect(),
            ));
        self.exposure
            .lock()
            .expect("exposure lock")
            .push((spec.operation_id.clone().unwrap_or_default(), spec.exposed));
    }
    fn ensure_schema_raw(
        &self,
        root_name: &str,
        _schemas: Vec<(
            String,
            utoipa::openapi::RefOr<utoipa::openapi::schema::Schema>,
        )>,
    ) -> String {
        root_name.to_owned()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// A router with optional admission workers and its database directory.
struct TestApi {
    router: Router,
    _handle: Option<toolkit_db::outbox::OutboxHandle>,
    _dir: common::TestDir,
}

impl std::ops::Deref for TestApi {
    type Target = Router;

    fn deref(&self) -> &Router {
        &self.router
    }
}

/// A router with both services wired, as `register_rest` builds it.
async fn router_with_db() -> TestApi {
    router_with(false).await
}

/// The same router with the legacy service ready, so v1 answers instead of refusing
/// on `is_ready()`.
async fn router_with_v1_ready() -> TestApi {
    router_with(true).await
}

async fn router_with(v1_ready: bool) -> TestApi {
    router_and_db_with(v1_ready).await.0
}

/// The router plus the provider behind it, for the discovery tests that seed
/// rows directly, without outbox workers competing for `SQLite`'s write lock.
async fn router_and_db() -> (TestApi, Arc<DBProvider<DbError>>) {
    router_and_db_configured(false, TypesRegistryConfig::default()).await
}

async fn router_and_db_with(v1_ready: bool) -> (TestApi, Arc<DBProvider<DbError>>) {
    router_and_db_setup(v1_ready, TypesRegistryConfig::default(), true).await
}

async fn router_and_db_configured(
    v1_ready: bool,
    config: TypesRegistryConfig,
) -> (TestApi, Arc<DBProvider<DbError>>) {
    router_and_db_setup(v1_ready, config, false).await
}

async fn router_and_db_setup(
    v1_ready: bool,
    config: TypesRegistryConfig,
    with_outbox: bool,
) -> (TestApi, Arc<DBProvider<DbError>>) {
    let dir = common::TestDir::new("tr-api");
    let dsn = format!(
        "sqlite://{}?mode=rwc&journal_mode=wal",
        dir.path().join("api.db").display()
    );
    let db = common::provider_for_with_outbox(&dsn, 8).await;
    let openapi = TestOpenApi::default();
    let legacy = Arc::new(TypesRegistryService::new(
        Arc::new(InMemoryGtsRepository::new(config.to_gts_config())),
        config.clone(),
    ));
    if v1_ready {
        legacy.switch_to_ready().expect("switch legacy to ready");
    }
    let dispatch = Arc::new(OutboxDispatch::new());
    let registry = Arc::new(RegistryService::new(
        db.db(),
        stores(),
        RegistrationPolicy::default(),
        config,
        Arc::clone(&dispatch) as Arc<dyn OperationDispatch>,
        common::metrics(),
    ));
    let handle = if with_outbox {
        Some(
            types_registry::infra::outbox::start(db.db(), &registry, &dispatch)
                .await
                .expect("start the admission outbox"),
        )
    } else {
        None
    };
    let router = types_registry::api::rest::routes::register_routes(
        Router::new(),
        &openapi,
        legacy,
        Some(registry),
    );
    let api = TestApi {
        router,
        _handle: handle,
        _dir: dir,
    };
    (api, db)
}

/// The same routes with no database bound — `no-db.yaml` and `--mock`. Ready,
/// because the point of the case is that v1 still serves.
fn router_without_db() -> Router {
    let openapi = TestOpenApi::default();
    let config = TypesRegistryConfig::default();
    let legacy = Arc::new(TypesRegistryService::new(
        Arc::new(InMemoryGtsRepository::new(config.to_gts_config())),
        config,
    ));
    legacy.switch_to_ready().expect("switch legacy to ready");
    types_registry::api::rest::routes::register_routes(Router::new(), &openapi, legacy, None)
}

fn schema(gts_id: &str) -> Value {
    json!({
        "$id": format!("gts://{gts_id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": { "name": { "type": "string" } },
    })
}

struct Response {
    status: StatusCode,
    content_type: Option<String>,
    location: Option<String>,
    retry_after: Option<String>,
    idempotency_replayed: Option<String>,
    cache_control: Option<String>,
    body: Value,
}

/// Dispatch, then wait for the operation a `202` receipt names. Most cases here
/// assert what the API reports once admission has run; [`call_raw`] opts out.
async fn call(router: &Router, req: Request<Body>) -> Response {
    let response = call_raw(router, req).await;
    if response.status == StatusCode::ACCEPTED && response.body["operation_id"].is_string() {
        await_operation(router, &response, "the outbox admits the submission").await;
    }
    response
}

async fn call_raw(router: &Router, req: Request<Body>) -> Response {
    let resp = router.clone().oneshot(req).await.expect("router dispatch");
    let status = resp.status();
    let content_type = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let location = resp
        .headers()
        .get(axum::http::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let retry_after = resp
        .headers()
        .get(axum::http::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let idempotency_replayed = resp
        .headers()
        .get("idempotency-replayed")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let cache_control = resp
        .headers()
        .get(axum::http::header::CACHE_CONTROL)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    Response {
        status,
        content_type,
        location,
        retry_after,
        idempotency_replayed,
        cache_control,
        body,
    }
}

fn assert_candidate_refusal(
    response: &Response,
    expected_resource: &str,
    expected_field: &str,
    expected_reason: &str,
) {
    assert_eq!(
        response.status,
        StatusCode::BAD_REQUEST,
        "got: {:?}",
        response.body
    );
    assert_eq!(
        response.content_type.as_deref(),
        Some("application/problem+json"),
    );
    assert_eq!(response.body["type"], json!(INVALID_ARGUMENT_TYPE));
    assert_eq!(
        response.body["context"]["resource_name"],
        json!(expected_resource),
        "an acceptance refusal names the candidate: {:?}",
        response.body,
    );
    let violations = response.body["context"]["field_violations"]
        .as_array()
        .expect("field_violations is an array");
    assert_eq!(violations.len(), 1, "got: {:?}", response.body);
    assert_eq!(violations[0]["field"], json!(expected_field));
    assert_eq!(violations[0]["reason"], json!(expected_reason));
}

/// A refusal the gear itself composed: no resource, one field violation naming the
/// request field the caller has to change.
///
/// Distinct from [`assert_invalid_argument_rejection`], which asserts the canonical
/// *extractor*'s shape — that one reports `cf.core.http.request` as the resource
/// type, and the two `400`s must stay tellable apart rather than both reading as
/// the domain's.
fn assert_field_refusal(response: &Response, expected_field: &str, expected_reason: &str) {
    assert_eq!(
        response.status,
        StatusCode::BAD_REQUEST,
        "got: {:?}",
        response.body
    );
    assert_eq!(
        response.content_type.as_deref(),
        Some("application/problem+json"),
    );
    assert_eq!(response.body["type"], json!(INVALID_ARGUMENT_TYPE));
    let violations = response.body["context"]["field_violations"]
        .as_array()
        .expect("field_violations is an array");
    assert_eq!(violations.len(), 1, "got: {:?}", response.body);
    assert_eq!(violations[0]["field"], json!(expected_field));
    assert_eq!(violations[0]["reason"], json!(expected_reason));
    assert!(
        violations[0]["description"]
            .as_str()
            .is_some_and(|description| !description.is_empty()),
        "the refusal carries a public description: {:?}",
        response.body,
    );
}

fn assert_invalid_argument_rejection(
    response: &Response,
    expected_status: StatusCode,
    expected_field: &str,
    expected_reason: &str,
) {
    assert_eq!(response.status, expected_status, "got: {:?}", response.body);
    assert_eq!(
        response.content_type.as_deref(),
        Some("application/problem+json"),
    );
    assert_eq!(response.body["type"], json!(INVALID_ARGUMENT_TYPE));
    assert_eq!(response.body["title"], json!("Invalid Argument"));
    assert_eq!(response.body["status"], json!(expected_status.as_u16()));
    assert_eq!(response.body["detail"], json!("Request validation failed"));
    assert_eq!(
        response.body["context"]["resource_type"],
        json!(HTTP_REQUEST_RESOURCE_TYPE),
    );

    let violations = response.body["context"]["field_violations"]
        .as_array()
        .expect("field_violations is an array");
    assert_eq!(violations.len(), 1, "got: {:?}", response.body);
    assert_eq!(violations[0]["field"], json!(expected_field));
    assert_eq!(violations[0]["reason"], json!(expected_reason));
    assert!(
        violations[0]["description"]
            .as_str()
            .is_some_and(|description| !description.is_empty()),
        "the rejection carries a public description: {:?}",
        response.body,
    );
    for forbidden in ["stack", "trace", "backtrace"] {
        assert!(
            response.body.get(forbidden).is_none(),
            "the Problem must not expose `{forbidden}`: {:?}",
            response.body,
        );
    }
}

fn submit(key: Option<&str>, body: &Value) -> Request<Body> {
    submit_to(&format!("{V2}/entities"), key, body)
}

fn submit_to(uri: &str, key: Option<&str>, body: &Value) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(key) = key {
        builder = builder.header("idempotency-key", key);
    }
    builder
        .body(Body::from(serde_json::to_vec(body).expect("serialize")))
        .expect("request")
}

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .expect("request")
}

fn one_candidate(gts_id: &str) -> Value {
    json!({ "items": [{ "gts_id": gts_id, "content": schema(gts_id) }] })
}

// ---------------------------------------------------------------------------
// Canonical extractor rejections
// ---------------------------------------------------------------------------

#[tokio::test]
async fn malformed_submission_json_is_a_canonical_invalid_argument() {
    let router = router_with_db().await;
    let request = Request::post(format!("{V2}/entities"))
        .header("content-type", "application/json")
        .header("idempotency-key", "malformed-json")
        .body(Body::from("{not-json}"))
        .expect("request");

    let response = call(&router, request).await;

    assert_invalid_argument_rejection(
        &response,
        StatusCode::BAD_REQUEST,
        "body",
        "json_syntax_error",
    );
}

#[tokio::test]
async fn invalid_list_query_is_a_canonical_invalid_argument() {
    let router = router_with_v1_ready().await;

    let response = call(&router, get(&format!("{V1}/entities?is_schema=not-a-bool"))).await;

    assert_invalid_argument_rejection(
        &response,
        StatusCode::BAD_REQUEST,
        "query",
        "invalid_query_string",
    );
}

#[tokio::test]
async fn invalid_operation_uuid_is_a_canonical_invalid_argument() {
    let router = router_with_db().await;

    let response = call(&router, get(&format!("{V2}/operations/not-a-uuid"))).await;

    assert_invalid_argument_rejection(
        &response,
        StatusCode::BAD_REQUEST,
        "path",
        "invalid_path_params",
    );
}

// ---------------------------------------------------------------------------
// The submit-then-poll contract
// ---------------------------------------------------------------------------

/// `202` with the operation's `Location` and an advisory `Retry-After`, then the
/// operation and the entity are both readable. This is Checkpoint 1's first item
/// as a single test.
#[tokio::test]
async fn a_registration_is_accepted_polled_and_read_back() {
    let router = router_with_db().await;

    let accepted = call(&router, submit(Some("key-1"), &one_candidate(CF_TYPE))).await;
    assert_eq!(accepted.status, StatusCode::ACCEPTED);
    let operation_id = accepted.body["operation_id"]
        .as_str()
        .expect("operation_id");
    let location = accepted
        .location
        .as_deref()
        .expect("a 202 carries Location");
    assert!(
        location.ends_with(&format!("{V2}/operations/{operation_id}")),
        "the receipt must point at the operation, prefix and all: {location}",
    );
    assert_eq!(
        accepted.retry_after.as_deref(),
        Some("1"),
        "advisory only, but present on 202",
    );
    assert_eq!(accepted.body["replayed"], json!(false));

    let operation = call(&router, get(&format!("{V2}/operations/{operation_id}"))).await;
    assert_eq!(operation.status, StatusCode::OK);
    assert_eq!(operation.body["status"], json!("completed"));
    assert_eq!(operation.body["kind"], json!("registration"));
    assert_eq!(operation.body["dry_run"], json!(false));
    let item = &operation.body["items"][0];
    assert_eq!(item["gts_id"], json!(CF_TYPE));
    assert_eq!(item["status"], json!("succeeded"));
    assert_eq!(item["resource_version"], json!(1));
    assert!(
        item.get("expected_resource_version").is_none(),
        "the operation outcome must not echo the request precondition",
    );
    assert!(
        item.get("revision_no").is_none(),
        "the operation outcome must expose resource_version, not an internal revision number",
    );

    let entity = call(
        &router,
        get(&format!(
            "{V2}/entities/{CF_TYPE}?$select=gts_id,kind,origin,content,resolved_schema,\
             effective_traits"
        )),
    )
    .await;
    assert_eq!(entity.status, StatusCode::OK);
    assert_eq!(entity.body["gts_id"], json!(CF_TYPE));
    assert_eq!(entity.body["kind"], json!("type_schema"));
    assert_eq!(entity.body["lifecycle_status"], json!("active"));
    assert_eq!(entity.body["origin"]["resource_version"], json!(1));
    // D3: the artifacts are materialized, so a read recomputes nothing.
    assert!(entity.body["resolved_schema"].is_object());
    assert!(entity.body["effective_traits"].is_object());
    assert!(entity.body["content"].is_object());
}

#[tokio::test]
async fn an_instance_reads_back_with_its_authored_value() {
    let router = router_with_db().await;
    call(&router, submit(Some("key-type"), &one_candidate(CF_TYPE))).await;

    let value = json!({ "name": "first" });
    let accepted = call(
        &router,
        submit(
            Some("key-instance"),
            &json!({ "items": [{ "gts_id": CF_INSTANCE, "content": value }] }),
        ),
    )
    .await;
    assert_eq!(accepted.status, StatusCode::ACCEPTED);

    let operation_id = accepted.body["operation_id"]
        .as_str()
        .expect("operation_id");
    let operation = call(&router, get(&format!("{V2}/operations/{operation_id}"))).await;
    assert_eq!(operation.body["items"][0]["status"], json!("succeeded"));
    assert!(operation.body["items"][0].get("revision_no").is_none());

    let entity = call(
        &router,
        get(&format!(
            "{V2}/entities/{CF_INSTANCE}?$select=gts_id,kind,origin,content,resolved_schema,\
             effective_traits,effective_traits_schema"
        )),
    )
    .await;
    assert_eq!(entity.status, StatusCode::OK);
    assert_eq!(entity.body["gts_id"], json!(CF_INSTANCE));
    assert_eq!(entity.body["kind"], json!("instance"));
    assert_eq!(entity.body["lifecycle_status"], json!("active"));
    assert_eq!(entity.body["origin"]["resource_version"], json!(1));
    assert_eq!(
        entity.body["content"], value,
        "the authored value, byte for byte what was submitted",
    );
    assert!(
        entity.body.get("revision_no").is_none(),
        "the immutable content revision remains internal; writes use resource_version",
    );
    for field in [
        "resolved_schema",
        "effective_traits",
        "effective_traits_schema",
    ] {
        assert!(
            entity.body.get(field).is_none(),
            "an Instance has no derived artifacts, so even selected they are absent, \
             not null: {:?}",
            entity.body,
        );
    }
}

#[tokio::test]
async fn an_instance_is_readable_by_registry_reference() {
    let router = router_with_db().await;
    call(&router, submit(Some("key-type"), &one_candidate(CF_TYPE))).await;
    call(
        &router,
        submit(
            Some("key-instance"),
            &json!({ "items": [{ "gts_id": CF_INSTANCE, "content": { "name": "first" } }] }),
        ),
    )
    .await;

    let uuid = gts::GtsId::try_new(CF_INSTANCE)
        .expect("identifier")
        .to_uuid();
    let by_uuid = call(
        &router,
        get(&format!("{V2}/entities/{uuid}?$select=gts_id,content")),
    )
    .await;
    assert_eq!(by_uuid.status, StatusCode::OK);
    assert_eq!(by_uuid.body["gts_id"], json!(CF_INSTANCE));
    assert_eq!(by_uuid.body["content"], json!({ "name": "first" }));
}

/// The same entity by its Registry Reference. Both keys name one row, which is why
/// the route takes `{entity_key}` rather than `{gts_id}`.
#[tokio::test]
async fn an_entity_is_readable_by_registry_reference() {
    let router = router_with_db().await;
    call(&router, submit(Some("key-1"), &one_candidate(CF_TYPE))).await;

    let uuid = gts::GtsId::try_new(CF_TYPE).expect("identifier").to_uuid();
    let by_uuid = call(&router, get(&format!("{V2}/entities/{uuid}"))).await;
    assert_eq!(by_uuid.status, StatusCode::OK);
    assert_eq!(by_uuid.body["gts_id"], json!(CF_TYPE));
    assert_eq!(by_uuid.body["gts_uuid"], json!(uuid.to_string()));
}

/// A replay of a terminal operation is `200`, not `202`: there is nothing left to
/// wait for, and `202` would tell the caller otherwise.
#[tokio::test]
async fn a_terminal_replay_answers_200() {
    let router = router_with_db().await;
    let first = call(&router, submit(Some("key-1"), &one_candidate(CF_TYPE))).await;
    assert_eq!(first.status, StatusCode::ACCEPTED);
    assert!(
        first.idempotency_replayed.is_none(),
        "a fresh submission must not be marked as replayed",
    );

    let replay = call(&router, submit(Some("key-1"), &one_candidate(CF_TYPE))).await;
    assert_eq!(replay.status, StatusCode::OK);
    assert_eq!(replay.body["replayed"], json!(true));
    assert_eq!(replay.body["operation_id"], first.body["operation_id"]);
    assert!(
        replay.retry_after.is_none(),
        "there is nothing to retry after",
    );
    assert_eq!(
        replay.idempotency_replayed.as_deref(),
        Some("true"),
        "an idempotent replay must carry the standard response signal",
    );
}

/// A different body under one key is a conflict, as an RFC-9457 problem document.
#[tokio::test]
async fn a_different_request_under_one_key_is_a_conflict_problem() {
    let router = router_with_db().await;
    call(&router, submit(Some("key-1"), &one_candidate(CF_TYPE))).await;

    let mut different = schema(CF_TYPE);
    different["title"] = json!("something else");
    let conflict = call(
        &router,
        submit(
            Some("key-1"),
            &json!({ "items": [{ "gts_id": CF_TYPE, "content": different }] }),
        ),
    )
    .await;
    assert_eq!(conflict.status, StatusCode::CONFLICT);
    assert!(
        conflict.body["type"].is_string() && conflict.body["title"].is_string(),
        "errors are RFC-9457 problem details, not raw status tuples: {:?}",
        conflict.body,
    );
}

#[tokio::test]
async fn the_receipt_is_followable_under_a_gateway_prefix() {
    let api = router_with_db().await;
    let prefixed = Router::new().nest("/cf", api.router.clone());

    let accepted = call(
        &prefixed,
        submit_to(
            &format!("/cf{V2}/entities"),
            Some("key-1"),
            &one_candidate(CF_TYPE),
        ),
    )
    .await;
    assert_eq!(accepted.status, StatusCode::ACCEPTED);
    let location = accepted
        .location
        .as_deref()
        .expect("a 202 carries Location");
    assert_eq!(
        location,
        format!(
            "/cf{V2}/operations/{}",
            accepted.body["operation_id"]
                .as_str()
                .expect("operation_id")
        ),
    );

    let followed = call(&prefixed, get(location)).await;
    assert_eq!(
        followed.status,
        StatusCode::OK,
        "a client that follows the receipt must reach the operation",
    );
    assert_eq!(followed.body["status"], json!("completed"));

    let unprefixed = call(
        &prefixed,
        get(&format!(
            "{V2}/operations/{}",
            accepted.body["operation_id"]
                .as_str()
                .expect("operation_id")
        )),
    )
    .await;
    assert_eq!(unprefixed.status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Refusals, all synchronous and all problem documents
// ---------------------------------------------------------------------------

/// The `Idempotency-Key` is required, and its absence is refused **before** any
/// operation exists — a generated key would turn every retry into a new operation.
#[tokio::test]
async fn a_missing_idempotency_key_is_a_synchronous_refusal() {
    let router = router_with_db().await;
    let refused = call(&router, submit(None, &one_candidate(CF_TYPE))).await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);
    assert!(refused.body["type"].is_string(), "{:?}", refused.body);

    // Nothing was accepted, so nothing is readable.
    let entity = call(&router, get(&format!("{V2}/entities/{CF_TYPE}"))).await;
    assert_eq!(entity.status, StatusCode::NOT_FOUND);
}

/// Dry Run is an ordinary accepted operation (T20): it gets a receipt and a
/// terminal outcome like any other, and it leaves nothing readable behind.
#[tokio::test]
async fn a_dry_run_is_accepted_and_writes_nothing() {
    let router = router_with_db().await;
    let mut body = one_candidate(CF_TYPE);
    body["dry_run"] = json!(true);

    let accepted = call(&router, submit(Some("dry-run-key"), &body)).await;
    assert_eq!(
        accepted.status,
        StatusCode::ACCEPTED,
        "a dry run is accepted like any other request: {:?}",
        accepted.body,
    );
    assert!(
        accepted.body.get("operation_id").is_some(),
        "and it is owed the receipt it asked for: {:?}",
        accepted.body,
    );

    let entity = call(&router, get(&format!("{V2}/entities/{CF_TYPE}"))).await;
    assert_eq!(
        entity.status,
        StatusCode::NOT_FOUND,
        "nothing a dry run evaluated is readable afterwards",
    );
}

/// A header that was sent but cannot be decoded is told apart from one that was not
/// sent at all: "required" would send the caller looking for a bug it does not have.
#[tokio::test]
async fn a_non_utf8_idempotency_key_is_not_reported_as_a_missing_one() {
    let router = router_with_db().await;
    let request = Request::builder()
        .method("POST")
        .uri(format!("{V2}/entities"))
        .header("content-type", "application/json")
        .header(
            "idempotency-key",
            axum::http::HeaderValue::from_bytes(&[0xff, 0xfe]).expect("a valid header value"),
        )
        .body(Body::from(
            serde_json::to_vec(&one_candidate(CF_TYPE)).expect("serialize"),
        ))
        .expect("request");

    let refused = call(&router, request).await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);
    let text = serde_json::to_string(&refused.body).expect("serialize");
    assert!(
        text.contains("not valid UTF-8"),
        "the detail must name what is wrong with the header: {text}",
    );
    assert!(
        !text.contains("required"),
        "a key that was sent must not be reported as missing: {text}",
    );
}

/// A closed region refuses a declared creation, and the problem document carries
/// the region and the parameter — the two things an operator has to edit.
#[tokio::test]
async fn a_closed_region_is_refused_with_the_region_named() {
    let router = router_with_db().await;
    let acme = gts_id!("acme.crm.customer.type.v1~");
    let refused = call(&router, submit(Some("key-1"), &one_candidate(acme))).await;
    // `failed_precondition` maps to 400 in this toolkit's canonical ladder, which
    // is the gRPC-to-HTTP convention. The status is not what carries the meaning
    // here — the precondition violation naming the region and the parameter is.
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);
    let text = serde_json::to_string(&refused.body).expect("serialize");
    assert!(
        text.contains("allowed_vendors"),
        "the parameter must be named: {text}",
    );
}

/// A literal `0` precondition is refused; omitting the field is how must-not-exist
/// is spelled.
#[tokio::test]
async fn a_zero_precondition_is_refused() {
    let router = router_with_db().await;
    let body = json!({
        "items": [{
            "gts_id": CF_TYPE,
            "content": schema(CF_TYPE),
            "expected_resource_version": 0,
        }]
    });
    let refused = call(&router, submit(Some("key-1"), &body)).await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);
}

/// A Type Schema's `$id` must be exactly `gts://<gts_id>` of its item. Every
/// other shape is a synchronous `400` naming the candidate and the `entity`
/// field, before any `202` or operation exists.
#[tokio::test]
async fn a_type_schema_id_that_does_not_name_its_item_is_refused_synchronously() {
    let router = router_with_db().await;
    let cases = [
        ("absent", None),
        ("null", Some(Value::Null)),
        ("a number", Some(json!(7))),
        ("an object", Some(json!({}))),
        ("empty", Some(json!(""))),
        ("malformed", Some(json!("gts://not a gts id"))),
        ("the bare canonical form", Some(json!(CF_TYPE))),
        (
            "another Type Schema",
            Some(json!(gts_uri!("cf.core.example.other.v1~"))),
        ),
        (
            "another major",
            Some(json!(gts_uri!("cf.core.example.type.v2~"))),
        ),
        ("padded", Some(json!(format!(" gts://{CF_TYPE}")))),
        (
            "oversized",
            Some(json!(format!("gts://{}", "x".repeat(64 * 1024)))),
        ),
    ];
    for (index, (label, declared)) in cases.into_iter().enumerate() {
        let mut content = schema(CF_TYPE);
        match declared {
            Some(value) => content["$id"] = value,
            None => {
                content.as_object_mut().expect("object").remove("$id");
            }
        }
        let body = json!({ "items": [{ "gts_id": CF_TYPE, "content": content }] });
        let refused = call_raw(&router, submit(Some(&format!("key-{index}")), &body)).await;
        assert_candidate_refusal(&refused, CF_TYPE, "entity", "VALIDATION_FAILED");
        let description = refused.body["context"]["field_violations"][0]["description"]
            .as_str()
            .unwrap_or_default();
        assert!(
            description.contains("$id") && description.contains(&format!("gts://{CF_TYPE}")),
            "{label}: the refusal names $id and the expected URI: {description}",
        );
        assert!(
            !description.contains("xxxx") && !description.contains("not a gts id"),
            "{label}: the declared $id is not echoed: {description}",
        );
    }

    // The same key, now with a matching `$id`, is a fresh acceptance: the refusals
    // bound no key and left nothing to replay.
    let accepted = call(&router, submit(Some("key-0"), &one_candidate(CF_TYPE))).await;
    assert_eq!(
        accepted.status,
        StatusCode::ACCEPTED,
        "got: {:?}",
        accepted.body
    );
    assert_eq!(accepted.body["replayed"], json!(false));
}

/// One mismatched Type Schema refuses the whole batch: no operation is written or
/// enqueued, and its valid neighbour is not registered on its own.
#[tokio::test]
async fn a_batch_with_one_mismatched_schema_id_writes_no_operation() {
    use sea_orm::EntityTrait;
    use toolkit_db::secure::SecureEntityExt;
    use types_registry::infra::storage::entity::operation;

    let (router, db) = router_and_db().await;
    let body = json!({
        "items": [
            { "gts_id": CF_TYPE, "content": schema(CF_TYPE) },
            { "gts_id": CF_OTHER, "content": schema(CF_THIRD) },
        ]
    });
    let refused = call_raw(&router, submit(Some("key-batch"), &body)).await;
    assert_candidate_refusal(&refused, CF_OTHER, "entity", "VALIDATION_FAILED");

    let conn = db.conn().expect("conn");
    let operations = operation::Entity::find()
        .secure()
        .scope_with(&common::allow_all())
        .all(&conn)
        .await
        .expect("read operations");
    assert!(
        operations.is_empty(),
        "a refused batch writes and enqueues no operation: {operations:?}",
    );
    let neighbour = call_raw(&router, get(&format!("{V2}/entities/{CF_TYPE}"))).await;
    assert_eq!(
        neighbour.status,
        StatusCode::NOT_FOUND,
        "the valid neighbour is not committed on its own",
    );
}

/// An Instance's identity lives in its item alone, so a value with an unrelated
/// `$id` is still admitted.
#[tokio::test]
async fn an_instance_is_not_held_to_the_schema_id_rule() {
    let router = router_with_db().await;
    let typed = call(&router, submit(Some("key-type"), &one_candidate(CF_TYPE))).await;
    assert_eq!(typed.status, StatusCode::ACCEPTED, "got: {:?}", typed.body);
    let type_operation = poll(&router, &typed).await;
    assert_eq!(
        type_operation["items"][0]["status"],
        json!("succeeded"),
        "the type is admitted before its Instance is submitted: {type_operation:?}",
    );

    let body = json!({
        "items": [{
            "gts_id": CF_INSTANCE,
            "content": { "$id": "urn:unrelated", "name": "first" },
        }]
    });
    let accepted = call(&router, submit(Some("key-instance"), &body)).await;
    assert_eq!(
        accepted.status,
        StatusCode::ACCEPTED,
        "got: {:?}",
        accepted.body
    );
    let operation = poll(&router, &accepted).await;
    assert_eq!(
        operation["items"][0]["status"],
        json!("succeeded"),
        "got: {operation:?}"
    );
}

#[tokio::test]
async fn naming_a_version_does_not_get_a_candidate_past_a_closed_region() {
    let router = router_with_db().await;
    let acme = gts_id!("acme.crm.customer.type.v1~");
    let body = json!({
        "items": [{
            "gts_id": acme,
            "content": schema(acme),
            "expected_resource_version": 7,
        }]
    });

    let accepted = call(&router, submit(Some("key-1"), &body)).await;
    assert_eq!(accepted.status, StatusCode::ACCEPTED);
    let operation_id = accepted.body["operation_id"]
        .as_str()
        .expect("operation_id");

    let operation = call(&router, get(&format!("{V2}/operations/{operation_id}"))).await;
    let item = &operation.body["items"][0];
    assert_eq!(item["status"], json!("failed"));
    let text = serde_json::to_string(&item["error"]).expect("serialize");
    assert!(
        text.contains("precondition_failed"),
        "the item must name the precondition it lost: {text}",
    );

    // The claim that matters: no entity exists in the closed region.
    let entity = call(&router, get(&format!("{V2}/entities/{acme}"))).await;
    assert_eq!(entity.status, StatusCode::NOT_FOUND);
}

/// The standing read criterion for anything that extends the write path: whatever
/// becomes storable is readable through the public route.
#[tokio::test]
async fn a_revision_is_readable_through_the_entity_route() {
    let router = router_with_db().await;
    call(&router, submit(Some("key-1"), &one_candidate(CF_TYPE))).await;

    let mut revised = schema(CF_TYPE);
    revised["title"] = json!("revised");
    let body = json!({
        "items": [{
            "gts_id": CF_TYPE,
            "content": revised,
            "expected_resource_version": 1,
        }]
    });
    let accepted = call(&router, submit(Some("key-2"), &body)).await;
    assert_eq!(accepted.status, StatusCode::ACCEPTED);
    let operation_id = accepted.body["operation_id"]
        .as_str()
        .expect("operation_id");
    let operation = call(&router, get(&format!("{V2}/operations/{operation_id}"))).await;
    assert_eq!(operation.body["items"][0]["status"], json!("succeeded"));
    assert_eq!(operation.body["items"][0]["resource_version"], json!(2));

    let entity = call(
        &router,
        get(&format!(
            "{V2}/entities/{CF_TYPE}?$select=origin,content,resolved_schema"
        )),
    )
    .await;
    assert_eq!(entity.status, StatusCode::OK);
    assert_eq!(entity.body["origin"]["resource_version"], json!(2));
    assert_eq!(entity.body["content"]["title"], json!("revised"));
    assert_eq!(
        entity.body["resolved_schema"]["title"],
        json!("revised"),
        "the artifacts were re-materialized with the pointer, not left on revision 1",
    );
}

/// Resubmitting the current content is terminal and successful, and says so with
/// its own status rather than as a second `succeeded`.
#[tokio::test]
async fn unchanged_content_reports_unchanged_on_the_operation() {
    let router = router_with_db().await;
    call(&router, submit(Some("key-1"), &one_candidate(CF_TYPE))).await;

    let body = json!({
        "items": [{
            "gts_id": CF_TYPE,
            "content": schema(CF_TYPE),
            "expected_resource_version": 1,
        }]
    });
    let accepted = call(&router, submit(Some("key-2"), &body)).await;
    let operation_id = accepted.body["operation_id"]
        .as_str()
        .expect("operation_id");
    let operation = call(&router, get(&format!("{V2}/operations/{operation_id}"))).await;
    let item = &operation.body["items"][0];
    assert_eq!(item["status"], json!("unchanged"));
    assert_eq!(item["resource_version"], json!(1));

    let entity = call(&router, get(&format!("{V2}/entities/{CF_TYPE}"))).await;
    assert_eq!(entity.body["origin"]["resource_version"], json!(1));
}

/// An absent operation and an absent entity are both `404` problem documents
/// rather than empty `200`s.
#[tokio::test]
async fn absent_resources_are_not_found_problems() {
    let router = router_with_db().await;

    let operation = call(
        &router,
        get(&format!(
            "{V2}/operations/00000000-0000-0000-0000-000000000001"
        )),
    )
    .await;
    assert_eq!(operation.status, StatusCode::NOT_FOUND);
    assert!(operation.body["type"].is_string());

    let entity = call(
        &router,
        get(&format!(
            "{V2}/entities/{}",
            gts_id!("cf.core.absent.type.v1~")
        )),
    )
    .await;
    assert_eq!(entity.status, StatusCode::NOT_FOUND);
    assert!(entity.body["type"].is_string());
}

/// A candidate that fails admission on its merits is **not** a failed request: the
/// submission is accepted, and the refusal is the item's outcome.
#[tokio::test]
async fn a_candidate_refused_by_admission_surfaces_through_the_operation() {
    let router = router_with_db().await;
    let dangling = json!({
        "$id": format!("gts://{CF_TYPE}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "allOf": [{ "$ref": format!("gts://{}", gts_id!("cf.core.absent.type.v1~")) }],
    });
    let accepted = call(
        &router,
        submit(
            Some("key-1"),
            &json!({ "items": [{ "gts_id": CF_TYPE, "content": dangling }] }),
        ),
    )
    .await;
    assert_eq!(accepted.status, StatusCode::ACCEPTED);

    let operation_id = accepted.body["operation_id"]
        .as_str()
        .expect("operation_id");
    let operation = call(&router, get(&format!("{V2}/operations/{operation_id}"))).await;
    assert_eq!(operation.body["status"], json!("completed"));
    let item = &operation.body["items"][0];
    assert_eq!(item["status"], json!("failed"));
    assert_eq!(
        item["error"]["reason"],
        json!("dependency_not_found"),
        "the reason travels as a field, not as prose: {:?}",
        item["error"],
    );
    assert_eq!(
        item["error"]["dependency_id"],
        gts_id!("cf.core.absent.type.v1~")
    );
    assert_eq!(item["error"]["dependency_kind"], "ref");
}

// ---------------------------------------------------------------------------
// No database bound
// ---------------------------------------------------------------------------

/// With no database bound the routes still exist and answer `503` — a problem
/// document naming the cause beats a `404` suggesting the API changed.
#[tokio::test]
async fn without_a_database_the_routes_report_service_unavailable() {
    let router = router_without_db();

    for req in [
        submit(Some("key-1"), &one_candidate(CF_TYPE)),
        get(&format!(
            "{V2}/operations/00000000-0000-0000-0000-000000000001"
        )),
        get(&format!(
            "{V2}/entities/{}",
            gts_id!("cf.core.example.type.v1~")
        )),
    ] {
        let resp = call(&router, req).await;
        assert_eq!(
            resp.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "got {:?}",
            resp.body
        );
    }
}

/// **The two stores do not see each other**, and neither route falls back to the
/// other on a miss. A fallback would report an entity as registered when the
/// admission meant to persist it never ran — the accident P6 exists to prevent.
#[tokio::test]
async fn a_v1_registration_is_absent_from_v2_and_the_reverse() {
    let router = router_with_v1_ready().await;

    // v1 registers into the in-memory repository, on `main`'s request shape.
    let v1_id = gts_id!("cf.core.example.v1only.v1~");
    let registered = call(
        &router,
        Request::post(format!("{V1}/entities"))
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "entities": [schema(v1_id)] }).to_string(),
            ))
            .expect("v1 request"),
    )
    .await;
    assert_eq!(
        registered.status,
        StatusCode::OK,
        "got {:?}",
        registered.body
    );
    assert_eq!(
        registered.body["summary"]["succeeded"], 1,
        "got {:?}",
        registered.body
    );

    // It is readable on v1 and absent on v2.
    assert_eq!(
        call(&router, get(&format!("{V1}/entities/{v1_id}")))
            .await
            .status,
        StatusCode::OK,
    );
    assert_eq!(
        call(&router, get(&format!("{V2}/entities/{v1_id}")))
            .await
            .status,
        StatusCode::NOT_FOUND,
        "a v1 registration must not be visible on the database surface",
    );

    // v2 admits into the database.
    let v2_id = gts_id!("cf.core.example.v2only.v1~");
    let accepted = call(&router, submit(Some("v2-only"), &one_candidate(v2_id))).await;
    assert_eq!(
        accepted.status,
        StatusCode::ACCEPTED,
        "got {:?}",
        accepted.body
    );

    // It is readable on v2 and absent on v1.
    assert_eq!(
        call(&router, get(&format!("{V2}/entities/{v2_id}")))
            .await
            .status,
        StatusCode::OK,
    );
    assert_eq!(
        call(&router, get(&format!("{V1}/entities/{v2_id}")))
            .await
            .status,
        StatusCode::NOT_FOUND,
        "a v2 admission must not be visible on the in-memory surface",
    );
}

/// With no database bound v2 degrades alone: a `--mock` or `no-db.yaml` deployment
/// keeps the contract it had before this branch.
#[tokio::test]
async fn without_a_database_the_v1_routes_still_serve() {
    let router = router_without_db();
    let id = gts_id!("cf.core.example.nodb.v1~");

    let registered = call(
        &router,
        Request::post(format!("{V1}/entities"))
            .header("content-type", "application/json")
            .body(Body::from(json!({ "entities": [schema(id)] }).to_string()))
            .expect("v1 request"),
    )
    .await;
    assert_eq!(
        registered.status,
        StatusCode::OK,
        "got {:?}",
        registered.body
    );
    assert_eq!(
        registered.body["summary"]["succeeded"], 1,
        "got {:?}",
        registered.body
    );

    assert_eq!(
        call(&router, get(&format!("{V1}/entities/{id}")))
            .await
            .status,
        StatusCode::OK,
    );
    assert_eq!(
        call(&router, get(&format!("{V1}/entities"))).await.status,
        StatusCode::OK,
    );
}

/// The `/cf/docs` check as a test. `OperationBuilder` registers two routes under one
/// `operation_id` without complaint and the second replaces the first in the
/// generated document — invisible in the router, which keys on method and path.
#[test]
fn both_versions_are_declared_with_distinct_operation_ids() {
    let openapi = TestOpenApi::default();
    let config = TypesRegistryConfig::default();
    let legacy = Arc::new(TypesRegistryService::new(
        Arc::new(InMemoryGtsRepository::new(config.to_gts_config())),
        config,
    ));
    let _router =
        types_registry::api::rest::routes::register_routes(Router::new(), &openapi, legacy, None);

    let declared = openapi.operations.lock().expect("operations lock").clone();

    let mut ids: Vec<&str> = declared.iter().map(|(_, _, id)| id.as_str()).collect();
    ids.sort_unstable();
    let total = ids.len();
    ids.dedup();
    assert_eq!(
        ids.len(),
        total,
        "duplicate operation id among {declared:?}"
    );

    let mut actual: Vec<(&str, &str, &str)> = declared
        .iter()
        .map(|(m, p, id)| (m.as_str(), p.as_str(), id.as_str()))
        .collect();
    actual.sort_unstable();

    let mut expected = vec![
        (
            "POST",
            "/types-registry/v1/entities",
            "types_registry.register",
        ),
        ("GET", "/types-registry/v1/entities", "types_registry.list"),
        (
            "GET",
            "/types-registry/v1/entities/{gts_id}",
            "types_registry.get",
        ),
        (
            "POST",
            "/types-registry/v2/entities",
            "types_registry.submit_entities",
        ),
        (
            "GET",
            "/types-registry/v2/operations/{operation_id}",
            "types_registry.get_operation",
        ),
        (
            "GET",
            "/types-registry/v2/entities/{entity_key}",
            "types_registry.get_entity",
        ),
        (
            "POST",
            "/types-registry/v2/entities:batchDelete",
            "types_registry.batch_delete_entities",
        ),
        (
            "DELETE",
            "/types-registry/v2/entities/{entity_key}",
            "types_registry.delete_entity",
        ),
        (
            "POST",
            "/types-registry/v2/entities:batchGet",
            "types_registry.batch_get_entities",
        ),
        (
            "GET",
            "/types-registry/v2/entities",
            "types_registry.list_entities",
        ),
    ];
    expected.sort_unstable();

    assert_eq!(actual, expected);
}

/// Ceiling C8's fail-closed fallback is executable: until a platform listener
/// authenticates and authorizes mutations, neither registration spelling may
/// be published through api-gateway.
#[test]
fn mutation_routes_are_internal_only() {
    let openapi = TestOpenApi::default();
    let config = TypesRegistryConfig::default();
    let legacy = Arc::new(TypesRegistryService::new(
        Arc::new(InMemoryGtsRepository::new(config.to_gts_config())),
        config,
    ));
    let _router =
        types_registry::api::rest::routes::register_routes(Router::new(), &openapi, legacy, None);

    let exposure = openapi.exposure.lock().expect("exposure lock");
    for operation_id in MUTATION_OPERATIONS {
        let exposed = exposure
            .iter()
            .find(|(id, _)| id == operation_id)
            .map(|(_, exposed)| *exposed)
            .expect("mutation operation is registered");
        assert!(
            !exposed,
            "{operation_id} must remain internal-only while ceiling C8 is open",
        );
    }
}

#[test]
fn the_idempotency_key_header_is_declared_as_a_required_parameter() {
    let openapi = TestOpenApi::default();
    let config = TypesRegistryConfig::default();
    let legacy = Arc::new(TypesRegistryService::new(
        Arc::new(InMemoryGtsRepository::new(config.to_gts_config())),
        config,
    ));
    let _router =
        types_registry::api::rest::routes::register_routes(Router::new(), &openapi, legacy, None);

    let params = openapi.params.lock().expect("params lock").clone();
    for operation_id in V2_MUTATION_OPERATIONS {
        let declared = params
            .iter()
            .find(|(id, _)| id == operation_id)
            .map(|(_, p)| p.clone())
            .expect("the mutation operation is registered");

        assert!(
            declared.contains(&(
                "Idempotency-Key".to_owned(),
                ParamLocation::Header,
                true,
                "string".to_owned(),
                None,
                None,
            )),
            "{operation_id} must declare a required Idempotency-Key header, got: {declared:?}",
        );
    }
}

#[test]
fn the_single_deletion_query_parameters_are_declared() {
    let openapi = TestOpenApi::default();
    let config = TypesRegistryConfig::default();
    let legacy = Arc::new(TypesRegistryService::new(
        Arc::new(InMemoryGtsRepository::new(config.to_gts_config())),
        config,
    ));
    let _router =
        types_registry::api::rest::routes::register_routes(Router::new(), &openapi, legacy, None);

    let params = openapi.params.lock().expect("params lock").clone();
    let declared = params
        .iter()
        .find(|(id, _)| id == "types_registry.delete_entity")
        .map(|(_, p)| p.clone())
        .expect("the single deletion operation is registered");

    for expected in [
        (
            "entity_key".to_owned(),
            ParamLocation::Path,
            true,
            "string".to_owned(),
            None,
            None,
        ),
        (
            "expected_resource_version".to_owned(),
            ParamLocation::Query,
            true,
            "integer".to_owned(),
            Some("int64".to_owned()),
            Some(1.0),
        ),
        (
            "dry_run".to_owned(),
            ParamLocation::Query,
            false,
            "boolean".to_owned(),
            None,
            None,
        ),
    ] {
        assert!(
            declared.contains(&expected),
            "missing parameter {expected:?}: {declared:?}",
        );
    }
}

#[test]
fn submission_response_headers_are_declared() {
    let openapi = TestOpenApi::default();
    let config = TypesRegistryConfig::default();
    let legacy = Arc::new(TypesRegistryService::new(
        Arc::new(InMemoryGtsRepository::new(config.to_gts_config())),
        config,
    ));
    let _router =
        types_registry::api::rest::routes::register_routes(Router::new(), &openapi, legacy, None);

    let headers = openapi
        .response_headers
        .lock()
        .expect("response headers lock");
    for operation_id in V2_MUTATION_OPERATIONS {
        let declared = headers
            .iter()
            .find(|(id, _)| id == operation_id)
            .map(|(_, headers)| headers)
            .expect("the mutation operation is registered");

        for expected in [
            (202, "Location", ResponseHeaderType::String),
            (202, "Retry-After", ResponseHeaderType::Integer),
            (202, "Idempotency-Replayed", ResponseHeaderType::Boolean),
            (200, "Location", ResponseHeaderType::String),
            (200, "Idempotency-Replayed", ResponseHeaderType::Boolean),
        ] {
            assert!(
                declared.iter().any(|actual| {
                    actual.0 == expected.0 && actual.1 == expected.1 && actual.2 == expected.2
                }),
                "{operation_id} is missing response header {expected:?}: {declared:?}",
            );
        }
        assert!(
            !declared
                .iter()
                .any(|(status, name, _)| *status == 200 && name == "Retry-After"),
            "{operation_id}: a terminal replay must not advertise Retry-After: {declared:?}",
        );
    }
}

/// `extract::Json<T>` can reject a request before its handler with three statuses
/// that `standard_errors` intentionally does not add. Keep the generated contract
/// aligned with both JSON request extractors.
#[test]
fn json_extractor_error_statuses_are_declared_for_both_post_operations() {
    let openapi = TestOpenApi::default();
    let config = TypesRegistryConfig::default();
    let legacy = Arc::new(TypesRegistryService::new(
        Arc::new(InMemoryGtsRepository::new(config.to_gts_config())),
        config,
    ));
    let _router =
        types_registry::api::rest::routes::register_routes(Router::new(), &openapi, legacy, None);

    let responses = openapi.responses.lock().expect("responses lock");
    for operation_id in [
        "types_registry.register",
        "types_registry.submit_entities",
        "types_registry.batch_delete_entities",
        "types_registry.batch_get_entities",
    ] {
        let declared = responses
            .iter()
            .find(|(id, _)| id == operation_id)
            .map(|(_, responses)| responses)
            .expect("JSON operation is registered");
        for status in [413, 415, 422] {
            assert!(
                declared.contains(&(status, "application/problem+json".to_owned())),
                "{operation_id} must declare {status} as a Problem response: {declared:?}",
            );
        }
    }
}

#[tokio::test]
async fn deletion_receipts_are_followable_under_a_gateway_prefix() {
    let api = router_with_db().await;
    let prefixed = Router::new().nest("/cf", api.router.clone());
    let batch_key = gts_id!("cf.core.example.batch_prefixed.v1~");
    for request in [
        submit_to(
            &format!("/cf{V2}/entities"),
            Some("register-one"),
            &one_candidate(CF_TYPE),
        ),
        submit_to(
            &format!("/cf{V2}/entities"),
            Some("register-two"),
            &one_candidate(batch_key),
        ),
    ] {
        let registered = call(&prefixed, request).await;
        assert_eq!(
            registered.status,
            StatusCode::ACCEPTED,
            "{:?}",
            registered.body
        );
    }

    let single = Request::builder()
        .method("DELETE")
        .uri(format!(
            "/cf{V2}/entities/{CF_TYPE}?expected_resource_version=1"
        ))
        .header("idempotency-key", "delete-single")
        .body(Body::empty())
        .expect("request");
    let batch = submit_to(
        &format!("/cf{V2}/entities:batchDelete"),
        Some("delete-batch"),
        &one_target(batch_key, 1),
    );

    for (case, request) in [("single", single), ("batch", batch)] {
        let accepted = call(&prefixed, request).await;
        assert_eq!(
            accepted.status,
            StatusCode::ACCEPTED,
            "{case} deletion: {:?}",
            accepted.body,
        );
        let location = accepted
            .location
            .as_deref()
            .expect("a 202 carries Location");
        assert_eq!(
            location,
            format!(
                "/cf{V2}/operations/{}",
                accepted.body["operation_id"]
                    .as_str()
                    .expect("operation_id")
            ),
            "{case} deletion must keep the mount prefix",
        );

        let followed = call(&prefixed, get(location)).await;
        assert_eq!(
            followed.status,
            StatusCode::OK,
            "{case} deletion receipt must be followable",
        );
        assert_eq!(followed.body["kind"], json!("deletion"));
    }
}

fn batch_delete(key: Option<&str>, body: &Value) -> Request<Body> {
    submit_to(&format!("{V2}/entities:batchDelete"), key, body)
}

fn delete_one(key: Option<&str>, entity_key: &str, query: &str) -> Request<Body> {
    let mut builder = Request::builder()
        .method("DELETE")
        .uri(format!("{V2}/entities/{entity_key}{query}"));
    if let Some(key) = key {
        builder = builder.header("idempotency-key", key);
    }
    builder.body(Body::empty()).expect("request")
}

fn one_target(key: &str, expected_resource_version: i64) -> Value {
    json!({ "items": [{ "key": key, "expected_resource_version": expected_resource_version }] })
}

async fn register_entity(router: &Router, idempotency_key: &str, gts_id: &str) {
    let accepted = call(
        router,
        submit(Some(idempotency_key), &one_candidate(gts_id)),
    )
    .await;
    assert_eq!(
        accepted.status,
        StatusCode::ACCEPTED,
        "registering {gts_id}: {:?}",
        accepted.body,
    );
}

async fn poll(router: &Router, accepted: &Response) -> Value {
    let operation_id = accepted.body["operation_id"]
        .as_str()
        .expect("a receipt carries an operation_id");
    let operation = call(router, get(&format!("{V2}/operations/{operation_id}"))).await;
    assert_eq!(operation.status, StatusCode::OK, "{:?}", operation.body);
    operation.body
}

#[tokio::test]
async fn a_deletion_is_accepted_polled_and_leaves_a_tombstone() {
    let router = router_with_db().await;
    register_entity(&router, "register", CF_TYPE).await;

    let accepted = call(
        &router,
        delete_one(Some("delete"), CF_TYPE, "?expected_resource_version=1"),
    )
    .await;
    assert_eq!(
        accepted.status,
        StatusCode::ACCEPTED,
        "a deletion is accepted like any other mutation: {:?}",
        accepted.body,
    );
    let operation_id = accepted.body["operation_id"]
        .as_str()
        .expect("operation_id");
    let location = accepted
        .location
        .as_deref()
        .expect("a 202 carries Location");
    assert_eq!(
        location,
        format!("{V2}/operations/{operation_id}"),
        "the receipt must point at the operation",
    );
    assert_eq!(accepted.retry_after.as_deref(), Some("1"));

    let operation = poll(&router, &accepted).await;
    assert_eq!(operation["kind"], json!("deletion"));
    assert_eq!(operation["dry_run"], json!(false));
    assert_eq!(operation["status"], json!("completed"));
    let item = &operation["items"][0];
    assert_eq!(item["gts_id"], json!(CF_TYPE));
    assert_eq!(item["status"], json!("succeeded"));

    let entity = call(&router, get(&format!("{V2}/entities/{CF_TYPE}"))).await;
    assert_eq!(
        entity.status,
        StatusCode::OK,
        "a tombstone stays exact-readable: {:?}",
        entity.body,
    );
    assert_eq!(entity.body["lifecycle_status"], json!("deleted"));
}

#[tokio::test]
async fn a_batch_deletion_reports_outcomes_in_request_order() {
    let router = router_with_db().await;
    let second = gts_id!("cf.core.example.other.v1~");
    register_entity(&router, "register-1", CF_TYPE).await;
    register_entity(&router, "register-2", second).await;

    let body = json!({
        "items": [
            { "key": second, "expected_resource_version": 1 },
            { "key": CF_TYPE, "expected_resource_version": 1 },
        ]
    });
    let accepted = call(&router, batch_delete(Some("delete-both"), &body)).await;
    assert_eq!(accepted.status, StatusCode::ACCEPTED, "{:?}", accepted.body);

    let operation = poll(&router, &accepted).await;
    assert_eq!(operation["kind"], json!("deletion"));
    assert_eq!(
        operation["items"][0]["gts_id"],
        json!(second),
        "request order, not identifier order: {:?}",
        operation["items"],
    );
    assert_eq!(operation["items"][1]["gts_id"], json!(CF_TYPE));
    for index in 0..2 {
        assert_eq!(operation["items"][index]["status"], json!("succeeded"));
    }
}

#[tokio::test]
async fn deleting_by_registry_reference_reports_the_identifier() {
    let router = router_with_db().await;
    register_entity(&router, "register", CF_TYPE).await;
    let entity = call(&router, get(&format!("{V2}/entities/{CF_TYPE}"))).await;
    let reference = entity.body["gts_uuid"]
        .as_str()
        .expect("the read carries the Registry Reference")
        .to_owned();

    let accepted = call(
        &router,
        delete_one(Some("delete"), &reference, "?expected_resource_version=1"),
    )
    .await;
    assert_eq!(accepted.status, StatusCode::ACCEPTED, "{:?}", accepted.body);

    let operation = poll(&router, &accepted).await;
    assert_eq!(
        operation["items"][0]["gts_id"],
        json!(CF_TYPE),
        "the outcome is keyed by identifier even for a UUID submission: {:?}",
        operation["items"],
    );
    assert_eq!(operation["items"][0]["status"], json!("succeeded"));
}

#[tokio::test]
async fn a_batch_mixing_identifiers_and_references_pairs_every_outcome() {
    let router = router_with_db().await;

    register_entity(&router, "seed-a", CF_TYPE).await;
    register_entity(&router, "seed-b", CF_OTHER).await;
    register_entity(&router, "seed-c", CF_THIRD).await;

    let mut references = Vec::new();
    for id in [CF_OTHER, CF_THIRD] {
        let entity = call(&router, get(&format!("{V2}/entities/{id}"))).await;
        references.push(
            entity.body["gts_uuid"]
                .as_str()
                .expect("the read carries the Registry Reference")
                .to_owned(),
        );
    }

    let accepted = call(
        &router,
        batch_delete(
            Some("mixed-batch"),
            &json!({
                "items": [
                    { "key": CF_TYPE, "expected_resource_version": 1 },
                    { "key": references[0], "expected_resource_version": 1 },
                    { "key": references[1], "expected_resource_version": 1 },
                ]
            }),
        ),
    )
    .await;
    assert_eq!(accepted.status, StatusCode::ACCEPTED, "{:?}", accepted.body);

    let operation = poll(&router, &accepted).await;
    let items = operation["items"]
        .as_array()
        .expect("the operation carries its items");
    assert_eq!(items.len(), 3, "{items:?}");

    for (position, expected) in [CF_TYPE, CF_OTHER, CF_THIRD].iter().enumerate() {
        assert_eq!(
            items[position]["gts_id"],
            json!(expected),
            "item {position} is paired with the wrong target: {items:?}",
        );
        assert_eq!(
            items[position]["status"],
            json!("succeeded"),
            "item {position}: {items:?}",
        );
    }
}

#[tokio::test]
async fn an_unknown_registry_reference_is_a_not_found_problem() {
    let router = router_with_db().await;
    let reference = uuid::Uuid::new_v4().to_string();

    let refused = call(
        &router,
        delete_one(Some("delete"), &reference, "?expected_resource_version=1"),
    )
    .await;
    assert_eq!(refused.status, StatusCode::NOT_FOUND, "{:?}", refused.body);
    assert_eq!(
        refused.content_type.as_deref(),
        Some("application/problem+json"),
    );

    let batched = call(
        &router,
        batch_delete(Some("batch"), &one_target(&reference, 1)),
    )
    .await;
    assert_eq!(
        batched.status,
        StatusCode::NOT_FOUND,
        "one deletion model, one answer: {:?}",
        batched.body,
    );
}

#[tokio::test]
async fn deleting_an_absent_identifier_is_a_terminal_item_failure() {
    let router = router_with_db().await;

    let accepted = call(
        &router,
        delete_one(Some("delete"), CF_TYPE, "?expected_resource_version=1"),
    )
    .await;
    assert_eq!(accepted.status, StatusCode::ACCEPTED, "{:?}", accepted.body);

    let operation = poll(&router, &accepted).await;
    assert_eq!(operation["items"][0]["status"], json!("failed"));
    assert_eq!(
        operation["items"][0]["error"]["reason"],
        json!("precondition_failed"),
        "{:?}",
        operation["items"],
    );
}

#[tokio::test]
async fn batch_deletion_requires_a_positive_expected_resource_version() {
    let router = router_with_db().await;

    for (case, body) in [
        ("missing", json!({ "items": [{ "key": CF_TYPE }] })),
        ("zero", one_target(CF_TYPE, 0)),
        ("negative", one_target(CF_TYPE, -1)),
    ] {
        let refused = call(&router, batch_delete(Some(case), &body)).await;
        assert_candidate_refusal(
            &refused,
            CF_TYPE,
            "expected_resource_version",
            "VALIDATION_FAILED",
        );
    }
}

#[tokio::test]
async fn a_misspelled_dry_run_is_refused_rather_than_committed() {
    let router = router_with_db().await;
    register_entity(&router, "seed-unknown-field", CF_TYPE).await;

    let refused = call(
        &router,
        batch_delete(
            Some("camel-case-dry-run"),
            &json!({
                "items": [{ "key": CF_TYPE, "expected_resource_version": 1 }],
                "dryRun": true,
            }),
        ),
    )
    .await;
    assert_eq!(
        refused.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "an unrecognized body field on a destructive route must not commit: {:?}",
        refused.body,
    );
    assert!(
        format!("{:?}", refused.body).contains("dryRun"),
        "the refusal must name the field the client got wrong: {:?}",
        refused.body,
    );

    let read = call(&router, get(&format!("{V2}/entities/{CF_TYPE}"))).await;
    assert_eq!(read.status, StatusCode::OK, "{:?}", read.body);
}

#[tokio::test]
async fn an_unrecognized_deletion_query_parameter_is_refused() {
    let router = router_with_db().await;

    for case in [
        "?expected_resource_version=1&dryrun=true",
        "?expected_resource_version=1&dry-run=true",
    ] {
        let refused = call(&router, delete_one(Some(case), CF_TYPE, case)).await;
        assert_eq!(
            refused.status,
            StatusCode::BAD_REQUEST,
            "{case} must be refused, not defaulted to a committing deletion: {:?}",
            refused.body,
        );
    }
}

#[tokio::test]
async fn single_deletion_requires_a_positive_expected_resource_version() {
    let router = router_with_db().await;

    for (case, query) in [
        ("absent", ""),
        ("zero", "?expected_resource_version=0"),
        ("negative", "?expected_resource_version=-1"),
    ] {
        let refused = call(&router, delete_one(Some(case), CF_TYPE, query)).await;
        assert_candidate_refusal(
            &refused,
            CF_TYPE,
            "expected_resource_version",
            "VALIDATION_FAILED",
        );
        assert!(
            refused.body["context"]["field_violations"][0]["description"]
                .as_str()
                .is_some_and(|d| d.contains("expected_resource_version")),
            "the {case} refusal must name the precondition it is about: {:?}",
            refused.body,
        );
    }

    let malformed = call(
        &router,
        delete_one(
            Some("non-numeric"),
            CF_TYPE,
            "?expected_resource_version=seven",
        ),
    )
    .await;
    assert_invalid_argument_rejection(
        &malformed,
        StatusCode::BAD_REQUEST,
        "query",
        "invalid_query_string",
    );
}

#[tokio::test]
async fn single_deletion_refuses_an_if_match_header() {
    let router = router_with_db().await;
    let request = Request::builder()
        .method("DELETE")
        .uri(format!(
            "{V2}/entities/{CF_TYPE}?expected_resource_version=1"
        ))
        .header("idempotency-key", "delete")
        .header("if-match", "\"1\"")
        .body(Body::empty())
        .expect("request");

    let refused = call(&router, request).await;
    assert_eq!(
        refused.status,
        StatusCode::BAD_REQUEST,
        "{:?}",
        refused.body
    );
    let text = serde_json::to_string(&refused.body).expect("serialize");
    assert!(
        text.contains("If-Match"),
        "the refusal must name the header it refuses: {text}",
    );
    assert!(
        text.contains("expected_resource_version"),
        "and the parameter that replaces it: {text}",
    );
}

#[tokio::test]
async fn the_two_deletion_spellings_agree_on_a_version_mismatch() {
    let router = router_with_db().await;
    register_entity(&router, "register", CF_TYPE).await;

    let single = call(
        &router,
        delete_one(Some("single"), CF_TYPE, "?expected_resource_version=7"),
    )
    .await;
    assert_eq!(single.status, StatusCode::ACCEPTED, "{:?}", single.body);
    let single_item = poll(&router, &single).await["items"][0].clone();

    let batched = call(
        &router,
        batch_delete(Some("batched"), &one_target(CF_TYPE, 7)),
    )
    .await;
    assert_eq!(batched.status, StatusCode::ACCEPTED, "{:?}", batched.body);
    let batched_item = poll(&router, &batched).await["items"][0].clone();

    assert_eq!(
        single_item["error"]["reason"],
        json!("precondition_failed"),
        "{single_item:?}",
    );
    assert_eq!(
        single_item, batched_item,
        "DELETE is sugar over a one-item batch, so the outcomes must be identical",
    );

    let entity = call(&router, get(&format!("{V2}/entities/{CF_TYPE}"))).await;
    assert_eq!(entity.body["lifecycle_status"], json!("active"));
    assert_eq!(entity.body["origin"]["resource_version"], json!(1));
}

#[tokio::test]
async fn a_dry_run_deletion_predicts_and_the_commit_performs() {
    let router = router_with_db().await;
    register_entity(&router, "register", CF_TYPE).await;

    for (idempotency_key, request) in [
        (
            "dry-single",
            delete_one(
                Some("dry-single"),
                CF_TYPE,
                "?expected_resource_version=1&dry_run=true",
            ),
        ),
        (
            "dry-batch",
            batch_delete(
                Some("dry-batch"),
                &json!({
                    "items": [{ "key": CF_TYPE, "expected_resource_version": 1 }],
                    "dry_run": true,
                }),
            ),
        ),
    ] {
        let accepted = call(&router, request).await;
        assert_eq!(
            accepted.status,
            StatusCode::ACCEPTED,
            "{idempotency_key}: {:?}",
            accepted.body,
        );
        let operation = poll(&router, &accepted).await;
        assert_eq!(operation["kind"], json!("deletion"), "{idempotency_key}");
        assert_eq!(operation["dry_run"], json!(true), "{idempotency_key}");
        assert_eq!(operation["status"], json!("completed"), "{idempotency_key}");
        let item = &operation["items"][0];
        assert_eq!(item["gts_id"], json!(CF_TYPE), "{idempotency_key}");
        assert_eq!(item["status"], json!("succeeded"), "{idempotency_key}");

        let entity = call(&router, get(&format!("{V2}/entities/{CF_TYPE}"))).await;
        assert_eq!(
            entity.body["lifecycle_status"],
            json!("active"),
            "{idempotency_key} must leave the entity alone",
        );
        assert_eq!(
            entity.body["origin"]["resource_version"],
            json!(1),
            "{idempotency_key} must not advance resource_version",
        );
    }

    let committed = call(
        &router,
        delete_one(Some("commit"), CF_TYPE, "?expected_resource_version=1"),
    )
    .await;
    assert_eq!(
        committed.status,
        StatusCode::ACCEPTED,
        "{:?}",
        committed.body
    );
    assert_eq!(
        poll(&router, &committed).await["items"][0]["status"],
        json!("succeeded"),
    );
    let entity = call(&router, get(&format!("{V2}/entities/{CF_TYPE}"))).await;
    assert_eq!(entity.body["lifecycle_status"], json!("deleted"));
}

#[tokio::test]
async fn a_dry_run_registration_reaches_a_terminal_outcome_and_writes_nothing() {
    let router = router_with_db().await;
    let mut body = one_candidate(CF_TYPE);
    body["dry_run"] = json!(true);

    let accepted = call(&router, submit(Some("dry-run"), &body)).await;
    assert_eq!(accepted.status, StatusCode::ACCEPTED, "{:?}", accepted.body);

    let operation = poll(&router, &accepted).await;
    assert_eq!(operation["kind"], json!("registration"));
    assert_eq!(operation["dry_run"], json!(true));
    assert_eq!(operation["status"], json!("completed"));
    let item = &operation["items"][0];
    assert_eq!(item["status"], json!("succeeded"));
    assert!(
        item["resource_version"].is_null(),
        "a predicted creation has no resource_version to report: {item:?}",
    );

    assert_eq!(
        call(&router, get(&format!("{V2}/entities/{CF_TYPE}")))
            .await
            .status,
        StatusCode::NOT_FOUND,
    );

    let committed = call(&router, submit(Some("commit"), &one_candidate(CF_TYPE))).await;
    assert_eq!(
        committed.status,
        StatusCode::ACCEPTED,
        "{:?}",
        committed.body
    );
    let entity = call(&router, get(&format!("{V2}/entities/{CF_TYPE}"))).await;
    assert_eq!(entity.status, StatusCode::OK);
    assert_eq!(entity.body["origin"]["resource_version"], json!(1));
}

#[tokio::test]
async fn both_deletion_spellings_require_an_idempotency_key() {
    let router = router_with_db().await;

    let single = call(
        &router,
        delete_one(None, CF_TYPE, "?expected_resource_version=1"),
    )
    .await;
    assert_eq!(single.status, StatusCode::BAD_REQUEST, "{:?}", single.body);

    let batched = call(&router, batch_delete(None, &one_target(CF_TYPE, 1))).await;
    assert_eq!(
        batched.status,
        StatusCode::BAD_REQUEST,
        "{:?}",
        batched.body
    );
}

#[tokio::test]
async fn a_terminal_deletion_replay_answers_200_and_marks_the_replay() {
    let router = router_with_db().await;
    register_entity(&router, "register", CF_TYPE).await;

    let first = call(
        &router,
        delete_one(Some("delete"), CF_TYPE, "?expected_resource_version=1"),
    )
    .await;
    assert_eq!(first.status, StatusCode::ACCEPTED, "{:?}", first.body);
    assert_eq!(first.idempotency_replayed, None);

    let replay = call(
        &router,
        delete_one(Some("delete"), CF_TYPE, "?expected_resource_version=1"),
    )
    .await;
    assert_eq!(replay.status, StatusCode::OK, "{:?}", replay.body);
    assert_eq!(replay.idempotency_replayed.as_deref(), Some("true"));
    assert_eq!(replay.retry_after, None);
    assert_eq!(replay.body["operation_id"], first.body["operation_id"]);
    assert_eq!(replay.body["replayed"], json!(true));
}

#[tokio::test]
async fn reusing_one_key_for_a_dry_run_then_a_commit_is_a_conflict() {
    let router = router_with_db().await;
    register_entity(&router, "register", CF_TYPE).await;

    let dry = call(
        &router,
        delete_one(
            Some("one-key"),
            CF_TYPE,
            "?expected_resource_version=1&dry_run=true",
        ),
    )
    .await;
    assert_eq!(dry.status, StatusCode::ACCEPTED, "{:?}", dry.body);

    let commit = call(
        &router,
        delete_one(Some("one-key"), CF_TYPE, "?expected_resource_version=1"),
    )
    .await;
    assert_eq!(
        commit.status,
        StatusCode::CONFLICT,
        "a dry run and a commit are different requests: {:?}",
        commit.body,
    );

    let entity = call(&router, get(&format!("{V2}/entities/{CF_TYPE}"))).await;
    assert_eq!(entity.body["lifecycle_status"], json!("active"));
}

#[tokio::test]
async fn without_a_database_the_deletion_routes_report_service_unavailable() {
    let router = router_without_db();

    let single = call(
        &router,
        delete_one(Some("delete"), CF_TYPE, "?expected_resource_version=1"),
    )
    .await;
    assert_eq!(
        single.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{:?}",
        single.body
    );

    let batched = call(
        &router,
        batch_delete(Some("batch"), &one_target(CF_TYPE, 1)),
    )
    .await;
    assert_eq!(
        batched.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{:?}",
        batched.body
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mutation {
    Register,
    BatchDelete,
    DeleteOne,
}

async fn await_operation(router: &Router, receipt: &Response, what: &str) -> Value {
    let operation_id = receipt.body["operation_id"]
        .as_str()
        .expect("a receipt carries an operation_id")
        .to_owned();
    // The receipt's own `Location` keeps any gateway prefix the router is nested
    // under; a path rebuilt from `V2` would miss it.
    let uri = receipt
        .location
        .clone()
        .unwrap_or_else(|| format!("{V2}/operations/{operation_id}"));
    common::await_delivery(what, || async {
        let response = call_raw(router, get(&uri)).await;
        assert_eq!(response.status, StatusCode::OK, "{:?}", response.body);
        match response.body["status"].as_str() {
            Some("completed") => Some(response.body),
            Some("pending" | "running") => None,
            other => panic!("unexpected operation status {other:?}: {:?}", response.body),
        }
    })
    .await
}

#[tokio::test]
async fn every_mutation_reaches_a_terminal_outcome_through_the_outbox() {
    let cases = vec![
        (Mutation::Register, false),
        (Mutation::Register, true),
        (Mutation::BatchDelete, false),
        (Mutation::BatchDelete, true),
        (Mutation::DeleteOne, false),
        (Mutation::DeleteOne, true),
    ];

    for (mutation, dry_run) in cases {
        let case = format!("{mutation:?} dry_run={dry_run}");
        let router = router_with_db().await;

        let deleting = mutation != Mutation::Register;
        if deleting {
            let seeded = call(&router, submit(Some("arrange"), &one_candidate(CF_TYPE))).await;
            assert_eq!(
                seeded.status,
                StatusCode::ACCEPTED,
                "{case}: {:?}",
                seeded.body
            );
            let operation = await_operation(&router, &seeded, &format!("{case}: arrange")).await;
            assert_eq!(
                operation["items"][0]["status"],
                json!("succeeded"),
                "{case}"
            );
        }

        let request = match (mutation, dry_run) {
            (Mutation::Register, false) => submit(Some("act"), &one_candidate(CF_TYPE)),
            (Mutation::Register, true) => {
                let mut body = one_candidate(CF_TYPE);
                body["dry_run"] = json!(true);
                submit(Some("act"), &body)
            }
            (Mutation::BatchDelete, false) => batch_delete(Some("act"), &one_target(CF_TYPE, 1)),
            (Mutation::BatchDelete, true) => {
                let mut body = one_target(CF_TYPE, 1);
                body["dry_run"] = json!(true);
                batch_delete(Some("act"), &body)
            }
            (Mutation::DeleteOne, false) => {
                delete_one(Some("act"), CF_TYPE, "?expected_resource_version=1")
            }
            (Mutation::DeleteOne, true) => delete_one(
                Some("act"),
                CF_TYPE,
                "?expected_resource_version=1&dry_run=true",
            ),
        };

        let accepted = call_raw(&router, request).await;
        assert_eq!(
            accepted.status,
            StatusCode::ACCEPTED,
            "{case}: a dispatched submission is never terminal on return: {:?}",
            accepted.body,
        );
        assert_eq!(
            accepted.body["status"],
            json!("pending"),
            "{case}: the receipt must report queued work, not a completed pass",
        );

        let operation = await_operation(&router, &accepted, &case).await;
        assert_eq!(
            operation["kind"],
            json!(if deleting { "deletion" } else { "registration" }),
            "{case}",
        );
        assert_eq!(operation["dry_run"], json!(dry_run), "{case}");
        assert_eq!(
            operation["items"][0]["status"],
            json!("succeeded"),
            "{case}: {:?}",
            operation["items"],
        );

        let entity = call(&router, get(&format!("{V2}/entities/{CF_TYPE}"))).await;
        match (mutation, dry_run) {
            (Mutation::Register, false) => {
                assert_eq!(entity.status, StatusCode::OK, "{case}: {:?}", entity.body);
                assert_eq!(entity.body["lifecycle_status"], json!("active"), "{case}");
                assert_eq!(
                    entity.body["origin"]["resource_version"],
                    json!(1),
                    "{case}"
                );
            }
            (Mutation::Register, true) => {
                assert_eq!(
                    entity.status,
                    StatusCode::NOT_FOUND,
                    "{case}: a predicted registration leaves nothing readable",
                );
            }
            (_, false) => {
                assert_eq!(entity.status, StatusCode::OK, "{case}: {:?}", entity.body);
                assert_eq!(entity.body["lifecycle_status"], json!("deleted"), "{case}");
            }
            (_, true) => {
                assert_eq!(entity.status, StatusCode::OK, "{case}: {:?}", entity.body);
                assert_eq!(
                    entity.body["lifecycle_status"],
                    json!("active"),
                    "{case}: a predicted deletion leaves the entity alone",
                );
                assert_eq!(
                    entity.body["origin"]["resource_version"],
                    json!(1),
                    "{case}: and does not advance resource_version",
                );
            }
        }
    }
}

#[tokio::test]
async fn every_mutation_receipt_refuses_to_be_cached() {
    let router = router_with_db().await;

    let submitted = call(&router, submit(Some("cache-1"), &one_candidate(CF_TYPE))).await;
    assert_eq!(
        submitted.status,
        StatusCode::ACCEPTED,
        "{:?}",
        submitted.body
    );
    assert_eq!(submitted.cache_control.as_deref(), Some("no-store"));

    let replayed = call(&router, submit(Some("cache-1"), &one_candidate(CF_TYPE))).await;
    assert_eq!(
        replayed.body["replayed"],
        json!(true),
        "{:?}",
        replayed.body
    );
    assert_eq!(
        replayed.cache_control.as_deref(),
        Some("no-store"),
        "a replay is still a receipt",
    );

    let batch = call(
        &router,
        batch_delete(Some("cache-2"), &one_target(CF_TYPE, 1)),
    )
    .await;
    assert_eq!(batch.status, StatusCode::ACCEPTED, "{:?}", batch.body);
    assert_eq!(batch.cache_control.as_deref(), Some("no-store"));

    register_entity(&router, "cache-3", CF_OTHER).await;
    let single = call(
        &router,
        delete_one(Some("cache-4"), CF_OTHER, "?expected_resource_version=1"),
    )
    .await;
    assert_eq!(single.status, StatusCode::ACCEPTED, "{:?}", single.body);
    assert_eq!(single.cache_control.as_deref(), Some("no-store"));
}

#[tokio::test]
async fn the_operation_polling_response_refuses_to_be_cached() {
    let router = router_with_db().await;

    let accepted = call(&router, submit(Some("poll-cache"), &one_candidate(CF_TYPE))).await;
    assert_eq!(accepted.status, StatusCode::ACCEPTED, "{:?}", accepted.body);
    let operation_id = accepted.body["operation_id"]
        .as_str()
        .expect("a receipt carries an operation_id")
        .to_owned();

    let polled = call(&router, get(&format!("{V2}/operations/{operation_id}"))).await;
    assert_eq!(polled.status, StatusCode::OK, "{:?}", polled.body);
    assert_eq!(
        polled.body["status"],
        json!("completed"),
        "{:?}",
        polled.body,
    );
    assert_eq!(
        polled.cache_control.as_deref(),
        Some("no-store"),
        "a terminal operation is still one caller's document — it names that \
         caller's operation and its per-candidate errors — so no cache, shared \
         or private, may retain it for reuse: {:?}",
        polled.body,
    );
}
// ---------------------------------------------------------------------------
// The two read routes: `:batchGet` and discovery (T22a)
// ---------------------------------------------------------------------------
//
// Both are driven through `register_routes` like every other case here, so the
// assertions cover the declared paths, the extractors and the problem documents
// rather than the domain methods behind them.

/// A second Type Schema in the same namespace, one identifier ahead of
/// [`CF_TYPE`] in byte order (`o` < `t`), so discovery ordering is assertable.
const CF_OTHER_TYPE: &str = gts_id!("cf.core.example.other.v1~");
/// A well-formed identifier nothing ever registers.
const CF_ABSENT_TYPE: &str = gts_id!("cf.core.example.absent.v1~");
/// Not an identifier at all, and not a UUID: `EntityKey::parse` classifies it as
/// an identifier that cannot exist.
const IMPOSSIBLE_KEY: &str = "not-a-gts-id";

/// A keyset cursor carrying `"v": 2`. Decoded plaintext:
/// `{"v":2,"k":["gts.cf.core.example.type.v1~"],"o":"asc","s":"+gts_id","d":"fwd"}`.
/// Written as a literal rather than encoded in the test, because the point is a
/// version this build does not support and so cannot construct through `CursorV1`.
const CURSOR_VERSION_2: &str = "eyJ2IjoyLCJrIjpbImd0cy5jZi5jb3JlLmV4YW1wbGUudHlwZS52MX4iXSwibyI6ImFzYyIsInMiOiIrZ3RzX2lkIiwiZCI6ImZ3ZCJ9";

fn post(uri: &str, body: &Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(body).expect("serialize")))
        .expect("request")
}

/// `POST {V2}/entities:batchGet`. No `Idempotency-Key`: a read is not a mutation.
fn batch_get(body: &Value) -> Request<Body> {
    post(&format!("{V2}/entities:batchGet"), body)
}

/// One `items` envelope of unconditional keys.
fn keys(keys: &[&str]) -> Value {
    json!({ "items": keys.iter().map(|k| json!({ "key": k })).collect::<Vec<_>>() })
}

/// `GET {V2}/entities`, with the query string spelled by the caller.
fn discover(query: &str) -> Request<Body> {
    get(&format!("{V2}/entities{query}"))
}

/// Register [`CF_TYPE`] and an Instance of it, both terminal on return.
async fn register_type_and_instance(router: &Router) {
    register_entity(router, "arrange-type", CF_TYPE).await;
    let accepted = call(
        &router.clone(),
        submit(
            Some("arrange-instance"),
            &json!({ "items": [{ "gts_id": CF_INSTANCE, "content": { "name": "first" } }] }),
        ),
    )
    .await;
    assert_eq!(accepted.status, StatusCode::ACCEPTED, "{:?}", accepted.body);
    let operation = poll(router, &accepted).await;
    assert_eq!(operation["items"][0]["status"], json!("succeeded"));
}

/// The identifiers a discovery page returned, in page order.
fn page_ids(body: &Value) -> Vec<String> {
    body["items"]
        .as_array()
        .expect("a page carries an items array")
        .iter()
        .map(|item| {
            item["gts_id"]
                .as_str()
                .expect("every page item names its identifier")
                .to_owned()
        })
        .collect()
}

// --- `:batchGet` ------------------------------------------------------------

/// One explicit result per requested key, absence included, echoing the key it was
/// asked by and in request order (DESIGN §3.3).
#[tokio::test]
async fn a_batch_read_answers_every_key_including_the_absent_one() {
    let router = router_with_db().await;
    register_type_and_instance(&router).await;

    let mut body = keys(&[CF_ABSENT_TYPE, CF_TYPE, CF_INSTANCE]);
    body["$select"] = json!(
        "gts_id,kind,origin,content,resolved_schema,effective_traits,effective_traits_schema"
    );
    let response = call(&router, batch_get(&body)).await;

    assert_eq!(response.status, StatusCode::OK, "{:?}", response.body);
    let items = response.body["items"].as_array().expect("items").to_owned();
    assert_eq!(items.len(), 3, "one result per requested key: {items:?}");

    assert_eq!(items[0]["key"], json!(CF_ABSENT_TYPE));
    assert_eq!(items[0]["status"], json!("not_found"));
    assert!(
        items[0].get("entity").is_none() || items[0]["entity"].is_null(),
        "an absence carries no entity: {:?}",
        items[0],
    );

    assert_eq!(items[1]["key"], json!(CF_TYPE));
    assert_eq!(items[1]["status"], json!("found"));
    let schema = &items[1]["entity"];
    assert_eq!(schema["gts_id"], json!(CF_TYPE));
    assert_eq!(schema["kind"], json!("type_schema"));
    assert_eq!(schema["origin"]["resource_version"], json!(1));
    assert!(
        schema["content"].is_object()
            && schema["resolved_schema"].is_object()
            && schema["effective_traits"].is_object()
            && schema["effective_traits_schema"].is_object(),
        "a batch read returns every selected document, D3 artifacts included: {schema:?}",
    );

    assert_eq!(items[2]["key"], json!(CF_INSTANCE));
    assert_eq!(items[2]["status"], json!("found"));
    assert_eq!(items[2]["entity"]["content"], json!({ "name": "first" }));
}

/// Both key spellings resolve one row, and each result echoes the spelling it was
/// asked by — the two are not duplicates of each other.
#[tokio::test]
async fn a_batch_read_answers_identifiers_and_registry_references_alike() {
    let router = router_with_db().await;
    register_entity(&router, "arrange", CF_TYPE).await;
    let uuid = gts::GtsId::try_new(CF_TYPE)
        .expect("identifier")
        .to_uuid()
        .to_string();

    let response = call(&router, batch_get(&keys(&[&uuid, CF_TYPE]))).await;

    assert_eq!(response.status, StatusCode::OK, "{:?}", response.body);
    let items = response.body["items"].as_array().expect("items");
    assert_eq!(items.len(), 2, "{items:?}");
    assert_eq!(items[0]["key"], json!(uuid));
    assert_eq!(items[1]["key"], json!(CF_TYPE));
    for item in items {
        assert_eq!(item["status"], json!("found"), "{item:?}");
        assert_eq!(item["entity"]["gts_id"], json!(CF_TYPE));
        assert_eq!(item["entity"]["gts_uuid"], json!(uuid));
    }
}

/// A key named twice is one result: the answer is per key, not per mention.
#[tokio::test]
async fn duplicate_keys_collapse_to_one_result() {
    let router = router_with_db().await;
    register_entity(&router, "arrange", CF_TYPE).await;

    let response = call(
        &router,
        batch_get(&keys(&[CF_TYPE, CF_ABSENT_TYPE, CF_TYPE])),
    )
    .await;

    assert_eq!(response.status, StatusCode::OK, "{:?}", response.body);
    let items = response.body["items"].as_array().expect("items");
    assert_eq!(
        items
            .iter()
            .map(|item| item["key"].clone())
            .collect::<Vec<_>>(),
        vec![json!(CF_TYPE), json!(CF_ABSENT_TYPE)],
        "the duplicate collapses onto its first mention: {items:?}",
    );
}

/// An identifier that cannot exist is an absence, not a validation failure, and
/// both read surfaces say so the same way: the exact read answers with the same
/// problem it gives a well-formed absent identifier, and the batch reports
/// `not_found` for both. Neither surface validates the key and refuses early while
/// the other looks it up.
#[tokio::test]
async fn an_impossible_identifier_is_classified_alike_by_both_read_surfaces() {
    let router = router_with_db().await;

    let impossible = call(&router, get(&format!("{V2}/entities/{IMPOSSIBLE_KEY}"))).await;
    let absent = call(&router, get(&format!("{V2}/entities/{CF_ABSENT_TYPE}"))).await;
    assert_eq!(impossible.status, StatusCode::NOT_FOUND);
    assert_eq!(absent.status, impossible.status);
    assert_eq!(absent.content_type, impossible.content_type);
    assert_eq!(
        absent.body["type"], impossible.body["type"],
        "one problem type for 'no such entity', whatever the key looked like",
    );

    let batched = call(&router, batch_get(&keys(&[IMPOSSIBLE_KEY, CF_ABSENT_TYPE]))).await;
    assert_eq!(batched.status, StatusCode::OK, "{:?}", batched.body);
    let items = batched.body["items"].as_array().expect("items");
    assert_eq!(items[0]["key"], json!(IMPOSSIBLE_KEY));
    assert_eq!(items[0]["status"], json!("not_found"));
    assert_eq!(items[1]["status"], json!("not_found"));
}

/// One header cannot represent a batch of validators, so `If-None-Match` is refused
/// rather than ignored (DESIGN §3.3).
#[tokio::test]
async fn a_batch_read_refuses_an_if_none_match_header() {
    let router = router_with_db().await;

    let request = Request::builder()
        .method("POST")
        .uri(format!("{V2}/entities:batchGet"))
        .header("content-type", "application/json")
        .header("if-none-match", "\"anything\"")
        .body(Body::from(
            serde_json::to_vec(&keys(&[CF_TYPE])).expect("serialize"),
        ))
        .expect("request");
    let response = call(&router, request).await;

    assert_field_refusal(&response, "If-None-Match", "VALIDATION_FAILED");
}

/// An empty batch is an envelope error: there is no key to answer about.
#[tokio::test]
async fn an_empty_batch_read_is_refused() {
    let router = router_with_db().await;

    let response = call(&router, batch_get(&json!({ "items": [] }))).await;

    assert_batch_count_refused(&response, 0);
}

fn assert_batch_count_refused(response: &Response, count: usize) {
    assert_field_refusal(response, "items", "VALIDATION_FAILED");
    let description = &response.body["context"]["field_violations"][0]["description"];
    assert!(
        description
            .as_str()
            .is_some_and(|d| d.ends_with(&format!("this one named {count}"))),
        "{description}",
    );
}

/// The batch ceiling is **100 keys**, matching the write ceiling rather than
/// DESIGN §3.3's 500 (SPEC §9 ceiling C10).
///
/// Pinned as a literal on purpose: the boundary test below reads the constant, so
/// it would stay green through a value change. This is the test that fails if the
/// number moves, which is what makes the number a decision rather than a default.
#[test]
fn the_batch_read_ceiling_is_one_hundred_keys() {
    assert_eq!(MAX_BATCH_GET_KEYS, 100);
}

/// A batch read is bounded, like every other batch on this surface: exactly at the
/// ceiling is served, one key past it is refused.
///
/// Driven by the constant rather than by a literal, so the boundary stays asserted
/// wherever the ceiling sits.
#[tokio::test]
async fn a_batch_read_is_bounded_at_its_ceiling() {
    let router = router_with_db().await;
    let ids: Vec<String> = (0..=MAX_BATCH_GET_KEYS)
        .map(|i| format!("{}cf.core.example.k{i:04}.v1~", gts::GTS_ID_PREFIX))
        .collect();
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();

    let at_ceiling = call(&router, batch_get(&keys(&refs[..MAX_BATCH_GET_KEYS]))).await;
    assert_eq!(
        at_ceiling.status,
        StatusCode::OK,
        "exactly {MAX_BATCH_GET_KEYS} keys must be served: {:?}",
        at_ceiling.body,
    );
    assert_eq!(
        at_ceiling.body["items"].as_array().expect("items").len(),
        MAX_BATCH_GET_KEYS,
        "one result per requested key, even when every one is absent",
    );

    let past_ceiling = call(&router, batch_get(&keys(&refs))).await;
    assert_batch_count_refused(&past_ceiling, MAX_BATCH_GET_KEYS + 1);
}

/// Items past the ceiling are counted, not kept, and the refusal still names the
/// exact count.
#[tokio::test]
async fn an_oversized_batch_read_is_refused_with_its_exact_count() {
    let router = router_with_db().await;
    let items: Vec<Value> = (0..10_000)
        .map(|i| json!({ "key": format!("k{i}") }))
        .collect();

    let response = call(&router, batch_get(&json!({ "items": items }))).await;

    assert_batch_count_refused(&response, 10_000);
}

// --- discovery ---------------------------------------------------------------

/// A page carries identity and metadata only: no authored content and none of D3's
/// artifacts (§8.5, D12). Ordering is by canonical identifier.
#[tokio::test]
async fn a_discovery_page_is_content_free_and_ordered_by_identifier() {
    let router = router_with_db().await;
    register_entity(&router, "arrange-other", CF_OTHER_TYPE).await;
    register_type_and_instance(&router).await;

    let response = call(&router, discover("")).await;

    assert_eq!(response.status, StatusCode::OK, "{:?}", response.body);
    assert_eq!(
        page_ids(&response.body),
        vec![CF_OTHER_TYPE, CF_TYPE, CF_INSTANCE],
        "sorted by canonical identifier",
    );
    assert_eq!(response.body["page_info"]["limit"], json!(50));
    for item in response.body["items"].as_array().expect("items") {
        assert_eq!(
            field_names(item),
            DEFAULT_FIELDS,
            "the default page is the document-free default set, no validator: {item:?}",
        );
        assert_eq!(item["origin"]["type"], json!("managed"), "{item:?}");
    }
}

/// A tombstone stays exact-readable and is listed only on request (ADR-0008).
#[tokio::test]
async fn discovery_lists_tombstones_only_on_request() {
    let router = router_with_db().await;
    register_entity(&router, "arrange-other", CF_OTHER_TYPE).await;
    register_entity(&router, "arrange", CF_TYPE).await;
    let deleted = call(
        &router,
        delete_one(Some("delete-1"), CF_TYPE, "?expected_resource_version=1"),
    )
    .await;
    assert_eq!(deleted.status, StatusCode::ACCEPTED, "{:?}", deleted.body);
    assert_eq!(
        poll(&router, &deleted).await["items"][0]["status"],
        json!("succeeded")
    );

    for (query, want) in [
        ("?limit=1", vec![CF_OTHER_TYPE]),
        ("?limit=1&lifecycle_status=active", vec![CF_OTHER_TYPE]),
        ("?limit=1&lifecycle_status=deleted", vec![CF_TYPE]),
        (
            "?limit=1&lifecycle_status=all",
            vec![CF_OTHER_TYPE, CF_TYPE],
        ),
        ("?limit=1&lifecycle_status=deleted&kind=instance", vec![]),
        (
            "?limit=1&lifecycle_status=all&depth=1&kind=type_schema",
            vec![CF_OTHER_TYPE, CF_TYPE],
        ),
        (
            &format!("?limit=1&lifecycle_status=deleted&pattern={CF_OTHER_TYPE}"),
            vec![],
        ),
    ] {
        let items = traverse(&router, query).await;
        let ids: Vec<&str> = items
            .iter()
            .map(|i| i["gts_id"].as_str().expect("id"))
            .collect();
        assert_eq!(ids, want, "{query}");
        for item in &items {
            let status = if item["gts_id"] == CF_TYPE {
                "deleted"
            } else {
                "active"
            };
            assert_eq!(item["lifecycle_status"], status, "{query}: {item:?}");
        }
    }

    let exact = call(&router, get(&format!("{V2}/entities/{CF_TYPE}"))).await;
    assert_eq!(exact.status, StatusCode::OK, "{:?}", exact.body);
    assert_eq!(exact.body["lifecycle_status"], json!("deleted"));
}

/// The cursor traverses a stable set exactly once, and the traversal ends with a
/// page that carries no cursor.
#[tokio::test]
async fn a_cursor_traverses_the_set_exactly_once() {
    let (router, db) = router_and_db().await;
    let ids = seed_entities(&db, 5).await;

    let mut seen: Vec<String> = Vec::new();
    let mut query = "?limit=2".to_owned();
    let mut first_page = true;
    for _ in 0..8 {
        let page = call(&router, discover(&query)).await;
        assert_eq!(page.status, StatusCode::OK, "{:?}", page.body);
        assert!(page.body["items"].as_array().expect("items").len() <= 2);
        if first_page {
            assert_eq!(
                page.body["page_info"]["limit"],
                json!(2),
                "page_info.limit must reflect the caller-supplied limit, not the default",
            );
            first_page = false;
        }
        seen.extend(page_ids(&page.body));
        let Some(cursor) = page.body["page_info"]["next_cursor"].as_str() else {
            assert_eq!(seen, ids, "every row exactly once, in identifier order");
            return;
        };
        query = format!("?limit=2&cursor={cursor}");
    }
    panic!("the cursor walk did not terminate; saw {seen:?}");
}

/// `limit` defaults to `page_size_default` and may not exceed `page_size_max` (D12).
#[tokio::test]
async fn a_page_size_outside_the_configured_range_is_refused() {
    let router = router_with_db().await;

    for query in ["?limit=101", "?limit=0"] {
        let response = call(&router, discover(query)).await;
        assert_field_refusal(&response, "limit", "VALIDATION_FAILED");
    }
}

/// A batch key is bounded at 1024 bytes before it is classified.
#[tokio::test]
async fn a_batch_key_over_1024_bytes_is_refused() {
    let router = router_with_db().await;

    let at = "a".repeat(1024);
    let served = call(&router, batch_get(&json!({ "items": [{ "key": at }] }))).await;
    assert_eq!(served.status, StatusCode::OK, "{:?}", served.body);
    assert_eq!(served.body["items"][0]["status"], json!("not_found"));

    let over = "a".repeat(1025);
    let refused = call(&router, batch_get(&json!({ "items": [{ "key": over }] }))).await;
    assert_field_refusal(&refused, "key", "VALIDATION_FAILED");
}

/// An item's `if_none_match` has the key's bound: 1024 bytes are served, 1025 refused.
#[tokio::test]
async fn a_batch_validator_over_1024_bytes_is_refused() {
    let router = router_with_db().await;
    let item =
        |len: usize| json!({ "items": [{ "key": CF_TYPE, "if_none_match": "v".repeat(len) }] });

    let served = call(&router, batch_get(&item(1024))).await;
    assert_eq!(served.status, StatusCode::OK, "{:?}", served.body);
    assert_eq!(served.body["items"][0]["status"], json!("not_found"));

    let refused = call(&router, batch_get(&item(1025))).await;
    assert_field_refusal(&refused, "if_none_match", "VALIDATION_FAILED");
}

/// The exact read shares the batch key bound; a non-canonical spelling is absent.
#[tokio::test]
async fn an_exact_read_bounds_its_key_and_matches_only_the_canonical_spelling() {
    let router = router_with_db().await;
    register_type_and_instance(&router).await;

    let refused = call(&router, exact(&"a".repeat(1025), "")).await;
    assert_field_refusal(&refused, "key", "VALIDATION_FAILED");

    for key in [
        "a".repeat(1024),
        format!("{CF_TYPE}%20"),
        format!("{CF_TYPE}%C3%A9"),
    ] {
        let response = call(&router, exact(&key, "")).await;
        assert_eq!(
            response.status,
            StatusCode::NOT_FOUND,
            "{key}: {:?}",
            response.body
        );
    }
}

/// A pattern is bounded at 1024 bytes before `gts-rust` parses it.
#[tokio::test]
async fn a_pattern_over_1024_bytes_is_refused_before_compilation() {
    let router = router_with_db().await;

    let at = call(&router, discover(&format!("?pattern={}", "a".repeat(1024)))).await;
    assert_field_refusal(&at, "pattern", "INVALID_QUERY");

    let over = call(&router, discover(&format!("?pattern={}", "a".repeat(1025)))).await;
    assert_field_refusal(&over, "pattern", "VALIDATION_FAILED");
}

/// Configured page sizes govern the default, the ceiling and its refusal.
#[tokio::test]
async fn configured_page_sizes_govern_discovery() {
    let mut config = TypesRegistryConfig::default();
    config.limits.page_size_default = 20;
    config.limits.page_size_max = 40;
    let (router, _db) = router_and_db_configured(true, config).await;

    for (query, limit) in [("", 20), ("?limit=40", 40)] {
        let response = call(&router, discover(query)).await;
        assert_eq!(response.status, StatusCode::OK, "{:?}", response.body);
        assert_eq!(response.body["page_info"]["limit"], json!(limit), "{query}");
    }
    let refused = call(&router, discover("?limit=41")).await;
    assert_field_refusal(&refused, "limit", "VALIDATION_FAILED");
    assert_eq!(
        refused.body["context"]["field_violations"][0]["description"],
        json!("a page size must be between 1 and 40; this request asked for 41"),
    );
}

/// A `limit` beyond `u32` is refused with the value the caller sent.
#[tokio::test]
async fn a_page_size_beyond_u32_is_refused_verbatim() {
    let router = router_with_db().await;

    for limit in ["4294967296", "99999999999"] {
        let response = call(&router, discover(&format!("?limit={limit}"))).await;
        assert_field_refusal(&response, "limit", "VALIDATION_FAILED");
        assert_eq!(
            response.body["context"]["field_violations"][0]["description"],
            json!(format!(
                "a page size must be between 1 and 100; this request asked for {limit}"
            )),
        );
    }
}

/// The boundary value `limit=page_size_max` (100) is served, not refused.
#[tokio::test]
async fn a_page_size_at_the_configured_maximum_is_served() {
    let router = router_with_db().await;

    let response = call(&router, discover("?limit=100")).await;

    assert_eq!(
        response.status,
        StatusCode::OK,
        "limit=page_size_max must be served: {:?}",
        response.body,
    );
    assert_eq!(
        response.body["page_info"]["limit"],
        json!(100),
        "page_info.limit must reflect the caller-supplied value",
    );
}

/// `toolkit-odata` cursors are versioned, and an unknown version is refused rather
/// than read as a position this build understands.
#[tokio::test]
async fn an_unknown_cursor_version_is_refused() {
    let router = router_with_db().await;

    let response = call(&router, discover(&format!("?cursor={CURSOR_VERSION_2}"))).await;

    assert_field_refusal(&response, "cursor", "VALIDATION_FAILED");
}

/// The cursor binds the query it was issued for: replaying one under a different
/// pattern would splice two traversals.
#[tokio::test]
async fn a_cursor_is_refused_under_a_different_pattern() {
    let (router, db) = router_and_db().await;
    _ = seed_entities(&db, 3).await;

    let first = call(&router, discover("?limit=1")).await;
    assert_eq!(first.status, StatusCode::OK, "{:?}", first.body);
    let cursor = first.body["page_info"]["next_cursor"]
        .as_str()
        .expect("a short page carries a cursor")
        .to_owned();

    let pattern = format!("{}cf.core.example.*", gts::GTS_ID_PREFIX);
    let response = call(
        &router,
        discover(&format!("?limit=1&pattern={pattern}&cursor={cursor}")),
    )
    .await;

    assert_field_refusal(&response, "cursor", "VALIDATION_FAILED");
}

/// The pattern is parsed by `gts-rust`, and a string it refuses is a `400` naming
/// the parameter rather than an empty page.
#[tokio::test]
async fn an_unparsable_pattern_is_refused() {
    let router = router_with_db().await;

    let response = call(&router, discover("?pattern=not-a-pattern")).await;

    assert_field_refusal(&response, "pattern", "INVALID_QUERY");
}

/// The pattern is exact: neither the sibling type nor the other major of the
/// same type is on the page.
#[tokio::test]
async fn a_pattern_returns_exactly_its_matches() {
    let (router, db) = router_and_db().await;
    seed_ids(
        &db,
        &[CF_OTHER_TYPE, CF_TYPE, gts_id!("cf.core.example.type.v2~")],
    )
    .await;

    let page = call(&router, discover(&format!("?pattern={CF_TYPE}"))).await;

    assert_eq!(page.status, StatusCode::OK, "{:?}", page.body);
    assert_eq!(
        page_ids(&page.body),
        vec![CF_TYPE],
        "neither `other.v1~` nor `type.v2~` matches: {:?}",
        page.body,
    );
}

/// A minor in a non-last segment pins that minor; an absent one matches every
/// minor. Walked with `limit=1`, so the cursor carries the same filter.
#[tokio::test]
async fn a_minor_in_an_early_segment_is_matched_exactly() {
    const V1: &str = gts_id!("cf.core.example.type.v1~cf.core.example.child.v1~");
    const V1_2: &str = gts_id!("cf.core.example.type.v1.2~cf.core.example.child.v1~");
    const V1_2_LEAF: &str =
        gts_id!("cf.core.example.type.v1.2~cf.core.example.child.v1~cf.core.example.leaf.v1");
    const V1_2_OTHER: &str = gts_id!("cf.core.example.type.v1.2~cf.core.example.other.v1~");
    const V1_3: &str = gts_id!("cf.core.example.type.v1.3~cf.core.example.child.v1~");

    let (router, db) = router_and_db().await;
    seed_ids(&db, &[V1, V1_2, V1_2_LEAF, V1_2_OTHER, V1_3]).await;

    for (pattern, want) in [
        (
            gts_id!("cf.core.example.type.v1.2~cf.core.example.child.v1~"),
            vec![V1_2, V1_2_LEAF],
        ),
        (
            gts_id!("cf.core.example.type.v1.2~cf.core.example.child.*"),
            vec![V1_2, V1_2_LEAF],
        ),
        (
            gts_id!("cf.core.example.type.v1.2~*"),
            vec![V1_2, V1_2_LEAF, V1_2_OTHER],
        ),
        (
            gts_id!("cf.core.example.type.v1~cf.core.example.child.v1~"),
            vec![V1, V1_2, V1_2_LEAF, V1_3],
        ),
        (gts_id!("cf.core.example.type.v1.0~*"), vec![]),
    ] {
        let items = traverse(&router, &format!("?limit=1&pattern={pattern}")).await;
        let ids: Vec<&str> = items
            .iter()
            .map(|item| item["gts_id"].as_str().expect("id"))
            .collect();
        let mut want = want;
        want.sort_unstable();
        assert_eq!(ids, want, "{pattern}");
    }
}

/// A sparse pattern still answers on the first page: the single match sorts after
/// thousands of rows sharing its identifier prefix, and no cursor follows.
///
/// `v9~` sorts after every `v2xxxx~` in byte order.
#[tokio::test]
async fn a_sparse_pattern_returns_its_match_on_the_first_page() {
    const MATCH: &str = gts_id!("cf.core.example.type.v9~");
    const DECOYS: u32 = 2100;

    let (router, db) = router_and_db().await;
    let mut ids: Vec<String> = (0..DECOYS)
        .map(|i| format!("{}cf.core.example.type.v2{i:04}~", gts::GTS_ID_PREFIX))
        .collect();
    ids.push(MATCH.to_owned());
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    seed_ids(&db, &refs).await;

    let page = call(&router, discover(&format!("?limit=10&pattern={MATCH}"))).await;
    assert_eq!(page.status, StatusCode::OK, "{:?}", page.body);
    assert_eq!(page_ids(&page.body), vec![MATCH.to_owned()]);
    assert!(
        page.body["page_info"]["next_cursor"].is_null(),
        "no cursor after the last match: {:?}",
        page.body
    );
}

/// The read routes degrade with the database like every other v2 route.
#[tokio::test]
async fn without_a_database_the_read_routes_report_service_unavailable() {
    let router = router_without_db();

    for request in [batch_get(&keys(&[CF_TYPE])), discover("")] {
        let response = call(&router, request).await;
        assert_eq!(
            response.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{:?}",
            response.body
        );
        assert_eq!(
            response.content_type.as_deref(),
            Some("application/problem+json"),
        );
    }
}

/// Discovery declares the three parameters it binds, with their scalar types.
#[test]
fn the_discovery_query_parameters_are_declared() {
    let openapi = TestOpenApi::default();
    let config = TypesRegistryConfig::default();
    let legacy = Arc::new(TypesRegistryService::new(
        Arc::new(InMemoryGtsRepository::new(config.to_gts_config())),
        config,
    ));
    let _router =
        types_registry::api::rest::routes::register_routes(Router::new(), &openapi, legacy, None);

    let params = openapi.params.lock().expect("params lock").clone();
    let declared = params
        .iter()
        .find(|(id, _)| id == "types_registry.list_entities")
        .map(|(_, p)| p.clone())
        .expect("the discovery operation is registered");

    for expected in [
        (
            "pattern".to_owned(),
            ParamLocation::Query,
            false,
            "string".to_owned(),
            None,
            None,
        ),
        (
            "limit".to_owned(),
            ParamLocation::Query,
            false,
            "integer".to_owned(),
            None,
            Some(1.0),
        ),
        (
            "cursor".to_owned(),
            ParamLocation::Query,
            false,
            "string".to_owned(),
            None,
            None,
        ),
        (
            "$select".to_owned(),
            ParamLocation::Query,
            false,
            "string".to_owned(),
            None,
            None,
        ),
    ] {
        assert!(
            declared.contains(&expected),
            "missing parameter {expected:?}: {declared:?}",
        );
    }
    assert_eq!(declared.len(), 7, "nothing else is declared: {declared:?}");
}

/// Every one of the seven v2 operations answers with RFC-9457 problems, so a
/// generated client has an error shape for each.
#[test]
fn all_seven_v2_operations_declare_problem_responses() {
    const V2_OPERATIONS: [&str; 7] = [
        "types_registry.submit_entities",
        "types_registry.batch_delete_entities",
        "types_registry.delete_entity",
        "types_registry.batch_get_entities",
        "types_registry.get_entity",
        "types_registry.list_entities",
        "types_registry.get_operation",
    ];

    let openapi = TestOpenApi::default();
    let config = TypesRegistryConfig::default();
    let legacy = Arc::new(TypesRegistryService::new(
        Arc::new(InMemoryGtsRepository::new(config.to_gts_config())),
        config,
    ));
    let _router =
        types_registry::api::rest::routes::register_routes(Router::new(), &openapi, legacy, None);

    let responses = openapi.responses.lock().expect("responses lock");
    for operation_id in V2_OPERATIONS {
        let declared = responses
            .iter()
            .find(|(id, _)| id == operation_id)
            .map(|(_, responses)| responses)
            .expect("the v2 operation is registered");
        // `standard_errors` plus the unbound-database `503` every v2 route answers.
        for status in [400, 401, 403, 500, 503] {
            assert!(
                declared.contains(&(status, "application/problem+json".to_owned())),
                "{operation_id} must declare {status} as a Problem response: {declared:?}",
            );
        }
    }
}

// --- direct row seeding for the paging cases --------------------------------

/// `count` active Type Schema rows in one family, written straight to `entity`.
/// Returns their identifiers in byte order.
///
/// The admission path is the subject of every other test here; these cases are
/// about paging over rows.
async fn seed_entities(db: &Arc<DBProvider<DbError>>, count: u32) -> Vec<String> {
    let ids: Vec<String> = (0..count)
        .map(|i| format!("{}cf.core.example.seed.v{}~", gts::GTS_ID_PREFIX, i + 1))
        .collect();
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    seed_ids(db, &refs).await;
    let mut sorted = ids;
    sorted.sort();
    sorted
}

/// Insert the given identifiers as active global Type Schemas of one family.
async fn seed_ids(db: &Arc<DBProvider<DbError>>, ids: &[&str]) {
    use types_registry::domain::enums::{EntityKind, OwnershipScope};
    use types_registry::domain::ports::{NewEntity, NewRevision};
    use types_registry::infra::storage::repo::{EntityRepo, TypeSchemaRepo, VersionFamilyRepo};

    let now = time::OffsetDateTime::now_utc();
    let owned: Vec<String> = ids.iter().map(|id| (*id).to_owned()).collect();
    db.transaction(move |tx| {
        Box::pin(async move {
            let scope = common::allow_all();
            let (family, _) = VersionFamilyRepo::create_or_get(
                tx,
                &scope,
                "gts.cf.core.example.seed",
                OwnershipScope::Global,
                None,
                now,
            )
            .await
            .expect("family");
            for id in &owned {
                let row = EntityRepo::insert(
                    tx,
                    &scope,
                    NewEntity {
                        gts_uuid: uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, id.as_bytes()),
                        gts_id: id.clone(),
                        entity_kind: EntityKind::TypeSchema,
                        family_id: family.id,
                        ownership_scope: OwnershipScope::Global,
                        owner_tenant_id: None,
                        owning_gear: Some("types-registry".to_owned()),
                        now,
                    },
                )
                .await
                .unwrap_or_else(|e| panic!("seed {id}: {e}"))
                .expect("a fresh identifier inserts");
                // A discovery page checks the current revision behind each row, so a
                // seeded row needs the same current state admission would write.
                let item = common::seed_operation_item(tx, id, 1, now).await;
                let document = serde_json::to_string(&schema(id)).expect("schema json");
                TypeSchemaRepo::insert_revision(
                    tx,
                    &scope,
                    NewRevision {
                        entity_id: row.id,
                        revision_no: 1,
                        raw_schema: document.clone(),
                        gts_spec_version: gts::GTS_SPECIFICATION_VERSION.to_owned(),
                        gts_impl_version: gts::GTS_IMPLEMENTATION_VERSION.to_owned(),
                        compat_forced: false,
                        operation_item_id: item,
                        now,
                    },
                )
                .await
                .expect("seed revision");
                common::seed_current_type_schema(tx, row.id, 1, &document, now).await;
            }
            Ok::<(), DbError>(())
        })
    })
    .await
    .expect("seed entity rows");
}

// ---------------------------------------------------------------------------
// `$select` on the exact read and `:batchGet` (T22b)
// ---------------------------------------------------------------------------

const DEFAULT_FIELDS: [&str; 5] = ["gts_id", "gts_uuid", "kind", "lifecycle_status", "origin"];

fn exact(key: &str, query: &str) -> Request<Body> {
    get(&format!("{V2}/entities/{key}{query}"))
}

/// A batch read of `keys` under one body `$select`.
fn selective_batch(keys_: &[&str], select: &str) -> Request<Body> {
    let mut body = keys(keys_);
    body["$select"] = json!(select);
    batch_get(&body)
}

fn field_names(entity: &Value) -> Vec<String> {
    let mut names: Vec<String> = entity
        .as_object()
        .expect("an entity is an object")
        .keys()
        .cloned()
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn an_absent_select_returns_the_document_free_default_on_both_routes() {
    let (router, _db) = router_and_db_with(false).await;
    register_type_and_instance(&router).await;

    for key in [CF_TYPE, CF_INSTANCE] {
        let single = call(&router, exact(key, "")).await;
        assert_eq!(single.status, StatusCode::OK, "{:?}", single.body);
        assert_eq!(field_names(&single.body), DEFAULT_FIELDS, "{key}");

        let batch = call(&router, batch_get(&keys(&[key]))).await;
        assert_eq!(
            batch.body["items"][0]["entity"], single.body,
            "one key answers identically on both routes: {key}",
        );

        let explicit = call(
            &router,
            exact(key, "?$select=gts_id,gts_uuid,kind,origin,lifecycle_status"),
        )
        .await;
        assert_eq!(explicit.body, single.body, "absent equals explicit default");
    }

    let schema = call(&router, exact(CF_TYPE, "")).await.body;
    let origin = &schema["origin"];
    assert_eq!(origin["type"], json!("managed"));
    assert_eq!(origin["resource_version"], json!(1));
    for stamp in ["created_at", "updated_at"] {
        let text = origin[stamp].as_str().expect("an RFC 3339 timestamp");
        time::OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|e| panic!("{stamp} {text}: {e}"));
    }
    assert_eq!(
        field_names(origin),
        ["created_at", "resource_version", "type", "updated_at"],
    );
}

#[tokio::test]
async fn each_document_is_selected_alone() {
    let router = router_with_db().await;
    register_type_and_instance(&router).await;

    for field in [
        "content",
        "resolved_schema",
        "effective_traits",
        "effective_traits_schema",
    ] {
        let response = call(&router, exact(CF_TYPE, &format!("?$select={field}"))).await;
        assert_eq!(response.status, StatusCode::OK, "{:?}", response.body);
        let mut expected = vec![field, "gts_id", "gts_uuid", "kind", "lifecycle_status"];
        expected.sort_unstable();
        assert_eq!(field_names(&response.body), expected, "{field}");
        assert!(
            response.body[field].is_object(),
            "{field}: {:?}",
            response.body
        );
    }

    // Selecting only a document the Instance lacks still returns its identity,
    // kind and lifecycle on every read, and omits the document.
    let select = "resolved_schema";
    let single = call(&router, exact(CF_INSTANCE, &format!("?$select={select}"))).await;
    let batch = call(&router, selective_batch(&[CF_INSTANCE], select)).await;
    let page = call(
        &router,
        discover(&format!("?kind=instance&$select={select}")),
    )
    .await;
    for entity in [
        &single.body,
        &batch.body["items"][0]["entity"],
        &page.body["items"][0],
    ] {
        assert_eq!(
            field_names(entity),
            ["gts_id", "gts_uuid", "kind", "lifecycle_status"]
        );
        assert_eq!(entity["gts_id"], CF_INSTANCE);
        assert_eq!(entity["kind"], "instance");
        assert_eq!(entity["lifecycle_status"], "active");
    }
}

#[tokio::test]
async fn a_mixed_batch_omits_type_schema_documents_on_the_instance() {
    let router = router_with_db().await;
    register_type_and_instance(&router).await;

    let response = call(
        &router,
        selective_batch(&[CF_TYPE, CF_INSTANCE], "kind,content,resolved_schema"),
    )
    .await;
    assert_eq!(response.status, StatusCode::OK, "{:?}", response.body);
    let schema = &response.body["items"][0]["entity"];
    let instance = &response.body["items"][1]["entity"];
    assert_eq!(
        field_names(schema),
        [
            "content",
            "gts_id",
            "gts_uuid",
            "kind",
            "lifecycle_status",
            "resolved_schema"
        ]
    );
    assert_eq!(
        field_names(instance),
        ["content", "gts_id", "gts_uuid", "kind", "lifecycle_status"],
        "an inapplicable document is absent, not null",
    );
    assert_eq!(instance["content"], json!({ "name": "first" }));
}

#[tokio::test]
async fn provenance_is_one_group_and_null_where_inapplicable() {
    let router = router_with_db().await;
    register_type_and_instance(&router).await;

    let response = call(
        &router,
        selective_batch(&[CF_TYPE, CF_INSTANCE], "provenance"),
    )
    .await;
    let schema = &response.body["items"][0]["entity"]["provenance"];
    let instance = &response.body["items"][1]["entity"]["provenance"];
    for provenance in [schema, instance] {
        assert_eq!(
            field_names(provenance),
            ["compat_forced", "gts_impl_version", "gts_spec_version"],
            "attribution is internal until P1",
        );
        assert!(provenance["gts_spec_version"].is_string(), "{provenance:?}");
        assert!(provenance["gts_impl_version"].is_string(), "{provenance:?}");
    }
    assert_eq!(schema["compat_forced"], json!(false));
    assert!(
        instance["compat_forced"].is_null(),
        "an Instance has nothing to waive: {instance:?}",
    );
}

/// Identity, kind and lifecycle are mandatory, so a projected tombstone is not an
/// absence.
#[tokio::test]
async fn a_tombstone_selected_for_content_still_reports_its_lifecycle() {
    let router = router_with_db().await;
    register_entity(&router, "arrange", CF_TYPE).await;
    let deleted = call(
        &router,
        delete_one(Some("delete"), CF_TYPE, "?expected_resource_version=1"),
    )
    .await;
    assert_eq!(deleted.status, StatusCode::ACCEPTED, "{:?}", deleted.body);

    let single = call(&router, exact(CF_TYPE, "?$select=content")).await;
    assert_eq!(single.status, StatusCode::OK, "{:?}", single.body);
    assert_eq!(
        field_names(&single.body),
        ["content", "gts_id", "gts_uuid", "kind", "lifecycle_status"]
    );
    assert_eq!(single.body["kind"], json!("type_schema"));
    assert_eq!(single.body["lifecycle_status"], json!("deleted"));
    let batch = call(&router, selective_batch(&[CF_TYPE], "content")).await;
    assert_eq!(batch.body["items"][0]["status"], json!("found"));
    assert_eq!(batch.body["items"][0]["entity"], single.body);
}

#[tokio::test]
async fn an_absent_key_is_404_exact_and_not_found_in_a_batch_under_any_selection() {
    let router = router_with_db().await;
    let single = call(&router, exact(CF_ABSENT_TYPE, "?$select=content")).await;
    assert_eq!(single.status, StatusCode::NOT_FOUND, "{:?}", single.body);
    let batch = call(&router, selective_batch(&[CF_ABSENT_TYPE], "content")).await;
    assert_eq!(batch.body["items"][0]["status"], json!("not_found"));
    assert!(batch.body["items"][0].get("entity").is_none());
}

/// One normalization, one projection: every selection answers one key identically.
#[tokio::test]
async fn exact_and_batch_reads_agree_for_every_selection() {
    let router = router_with_db().await;
    register_type_and_instance(&router).await;
    let uuid = gts::GtsId::try_new(CF_TYPE).expect("identifier").to_uuid();

    for select in [
        "gts_id",
        "content,provenance",
        "RESOLVED_SCHEMA, effective_traits",
        "content,effective_traits,effective_traits_schema,gts_id,gts_uuid,kind,\
         lifecycle_status,origin,provenance,resolved_schema",
    ] {
        for key in [CF_TYPE.to_owned(), CF_INSTANCE.to_owned(), uuid.to_string()] {
            let query = format!("?$select={}", select.replace(' ', "%20"));
            let single = call(&router, exact(&key, &query)).await;
            assert_eq!(
                single.status,
                StatusCode::OK,
                "{select} {key}: {:?}",
                single.body
            );
            let batch = call(&router, selective_batch(&[&key], select)).await;
            assert_eq!(
                batch.body["items"][0]["entity"], single.body,
                "{select} {key}"
            );
        }
    }
}

#[tokio::test]
async fn invalid_selections_are_refused_on_both_routes() {
    let router = router_with_db().await;
    register_entity(&router, "arrange", CF_TYPE).await;

    for select in [
        "",
        "content,,kind",
        "content,",
        "content,Content",
        "contents",
        "availability",
        "owned_by_context_tenant",
        "content.title",
        "effective/resolved_schema",
        "key",
    ] {
        let query = format!("?$select={select}");
        let single = call(&router, exact(CF_TYPE, &query)).await;
        assert_eq!(
            single.status,
            StatusCode::BAD_REQUEST,
            "{select:?}: {:?}",
            single.body
        );
        assert_eq!(
            single.body["context"]["field_violations"][0]["field"],
            json!("$select"),
            "{select:?}: {:?}",
            single.body,
        );
        let batch = call(&router, selective_batch(&[CF_TYPE], select)).await;
        assert_field_refusal(&batch, "$select", "INVALID_SELECT");
    }
}

/// Parameters a route does not declare are refused, not silently ignored.
#[tokio::test]
async fn undeclared_query_parameters_are_refused_on_both_routes() {
    let router = router_with_db().await;
    register_entity(&router, "arrange", CF_TYPE).await;

    for (query, field) in [
        ("?pattern=gts.cf.*", "pattern"),
        ("?kind=type", "kind"),
        ("?is_schema=true", "is_schema"),
        ("?limit=1", "limit"),
        ("?$top=1", "$top"),
        ("?$filter=gts_id%20eq%20'x'", "$filter"),
        ("?$orderby=gts_id", "$orderby"),
        ("?$skip=1", "$skip"),
        ("?$expand=content", "$expand"),
    ] {
        let single = call(&router, exact(CF_TYPE, query)).await;
        assert_field_refusal(&single, field, "UNSUPPORTED_QUERY_PARAM");
    }

    let repeated = call(&router, exact(CF_TYPE, "?$select=content&$select=kind")).await;
    assert_field_refusal(&repeated, "$select", "VALIDATION_FAILED");

    let in_query = post(
        &format!("{V2}/entities:batchGet?$select=content"),
        &keys(&[CF_TYPE]),
    );
    let refused = call(&router, in_query).await;
    assert_field_refusal(&refused, "$select", "UNSUPPORTED_QUERY_PARAM");
}

/// A misspelled body field would otherwise be answered with the default set.
#[tokio::test]
async fn unknown_batch_body_fields_are_refused() {
    let router = router_with_db().await;
    for (body, field) in [
        (
            json!({ "items": [{ "key": CF_TYPE }], "select": "content" }),
            "select",
        ),
        (
            json!({ "items": [{ "key": CF_TYPE, "$select": "content" }] }),
            "$select",
        ),
    ] {
        let response = call(&router, batch_get(&body)).await;
        assert_eq!(
            response.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{body}: {:?}",
            response.body,
        );
        let violation = &response.body["context"]["field_violations"][0];
        assert_eq!(violation["reason"], json!("invalid_json_body"), "{body}");
        let description = violation["description"].as_str().unwrap_or_default();
        assert!(
            description.contains(&format!("unknown field `{field}`")),
            "the refusal names `{field}`: {description}",
        );
    }
}

#[test]
fn the_exact_read_declares_select() {
    let openapi = TestOpenApi::default();
    let _router = router_for_openapi(&openapi);
    let params = openapi.params.lock().expect("params lock");
    let (_, declared) = params
        .iter()
        .find(|(id, _)| id == "types_registry.get_entity")
        .expect("exact read registered");
    let names: Vec<&str> = declared
        .iter()
        .filter(|p| p.1 == ParamLocation::Query)
        .map(|p| p.0.as_str())
        .collect();
    assert_eq!(names, ["$select"]);
}

fn router_for_openapi(openapi: &TestOpenApi) -> Router {
    let config = TypesRegistryConfig::default();
    let legacy = Arc::new(TypesRegistryService::new(
        Arc::new(InMemoryGtsRepository::new(config.to_gts_config())),
        config,
    ));
    types_registry::api::rest::routes::register_routes(Router::new(), openapi, legacy, None)
}

// ---------------------------------------------------------------------------
// `$select` on discovery and its cursor binding (T22b)
// ---------------------------------------------------------------------------

/// Walk every page under `query`, returning the pages' items in order.
async fn traverse(router: &Router, query: &str) -> Vec<Value> {
    let mut items = Vec::new();
    let mut next: Option<String> = None;
    for _ in 0..10 {
        let uri = match &next {
            Some(cursor) => format!("{query}&cursor={cursor}"),
            None => query.to_owned(),
        };
        let page = call(router, discover(&uri)).await;
        assert_eq!(page.status, StatusCode::OK, "{uri}: {:?}", page.body);
        items.extend(
            page.body["items"]
                .as_array()
                .expect("items")
                .iter()
                .cloned(),
        );
        match page.body["page_info"]["next_cursor"].as_str() {
            Some(cursor) => next = Some(cursor.to_owned()),
            None => return items,
        }
    }
    panic!("the traversal did not end: {query}");
}

#[tokio::test]
async fn discovery_projects_selected_documents_across_pages() {
    let router = router_with_db().await;
    register_entity(&router, "arrange-other", CF_OTHER_TYPE).await;
    register_type_and_instance(&router).await;

    let items = traverse(&router, "?limit=1&$select=gts_id,content,resolved_schema").await;
    assert_eq!(
        items
            .iter()
            .map(|item| item["gts_id"].as_str().expect("selected gts_id"))
            .collect::<Vec<_>>(),
        [CF_OTHER_TYPE, CF_TYPE, CF_INSTANCE],
        "every active entity exactly once, in canonical order",
    );
    for item in &items[..2] {
        assert_eq!(
            field_names(item),
            [
                "content",
                "gts_id",
                "gts_uuid",
                "kind",
                "lifecycle_status",
                "resolved_schema"
            ]
        );
        assert!(item["content"].is_object() && item["resolved_schema"].is_object());
    }
    assert_eq!(
        field_names(&items[2]),
        ["content", "gts_id", "gts_uuid", "kind", "lifecycle_status"],
        "the Instance has no resolved_schema",
    );

    let exact = call(
        &router,
        exact(CF_INSTANCE, "?$select=gts_id,content,resolved_schema"),
    )
    .await;
    assert_eq!(
        items[2], exact.body,
        "a page item is the exact read's projection"
    );
}

#[tokio::test]
async fn a_page_under_a_metadata_only_selection_carries_only_it() {
    let router = router_with_db().await;
    register_type_and_instance(&router).await;
    let page = call(&router, discover("?$select=gts_uuid")).await;
    assert_eq!(page.status, StatusCode::OK, "{:?}", page.body);
    for item in page.body["items"].as_array().expect("items") {
        assert_eq!(
            field_names(item),
            ["gts_id", "gts_uuid", "kind", "lifecycle_status"]
        );
    }
}

async fn first_cursor(router: &Router, query: &str) -> String {
    let page = call(router, discover(query)).await;
    assert_eq!(page.status, StatusCode::OK, "{:?}", page.body);
    page.body["page_info"]["next_cursor"]
        .as_str()
        .expect("a one-item page of three carries a cursor")
        .to_owned()
}

#[tokio::test]
async fn a_cursor_is_refused_under_a_different_selection() {
    let (router, db) = router_and_db().await;
    _ = seed_entities(&db, 3).await;

    for (issued, resumed) in [
        ("?limit=1", "?limit=1&$select=content"),
        ("?limit=1&$select=content", "?limit=1"),
        (
            "?limit=1&$select=content",
            "?limit=1&$select=content,origin",
        ),
        ("?limit=1&$select=gts_id", "?limit=1&$select=origin"),
    ] {
        let cursor = first_cursor(&router, issued).await;
        let response = call(&router, discover(&format!("{resumed}&cursor={cursor}"))).await;
        assert_field_refusal(&response, "cursor", "VALIDATION_FAILED");
    }
}

/// The cursor binds the normalized set, never its spelling: resuming under an
/// equivalent spelling answers exactly what resuming under the original does.
#[tokio::test]
async fn equivalent_selections_resume_one_traversal() {
    let (router, db) = router_and_db().await;
    _ = seed_entities(&db, 3).await;

    for (issued, equivalent) in [
        (
            "?limit=1",
            "?limit=1&$select=gts_id,gts_uuid,kind,origin,lifecycle_status",
        ),
        (
            "?limit=1&$select=origin,lifecycle_status,kind,gts_uuid,gts_id",
            "?limit=1",
        ),
        (
            "?limit=1&$select=gts_uuid,content",
            "?limit=1&$select=Content,%20GTS_UUID,lifecycle_status",
        ),
        ("?limit=1&$select=gts_id", "?limit=1&$select=gts_uuid"),
        ("?limit=1&$select=gts_id", "?limit=1&$select=kind"),
        ("?limit=1&$select=content", "?limit=1&$select=content,kind"),
    ] {
        let cursor = first_cursor(&router, issued).await;
        let original = call(&router, discover(&format!("{issued}&cursor={cursor}"))).await;
        let respelled = call(&router, discover(&format!("{equivalent}&cursor={cursor}"))).await;
        assert_eq!(
            original.status,
            StatusCode::OK,
            "{issued}: {:?}",
            original.body
        );
        assert_eq!(
            respelled.status,
            StatusCode::OK,
            "{equivalent}: {:?}",
            respelled.body
        );
        assert_eq!(
            original.body["items"].as_array().map(Vec::len),
            Some(1),
            "{issued}: {:?}",
            original.body,
        );
        assert_eq!(
            respelled.body, original.body,
            "{issued} -> {equivalent}: same items and same next_cursor",
        );
    }
}

#[tokio::test]
async fn toolkit_spellings_of_limit_and_cursor_are_one_slot_each() {
    let (router, db) = router_and_db().await;
    let seeded = seed_entities(&db, 3).await;

    let page = call(&router, discover("?$top=1")).await;
    assert_eq!(page_ids(&page.body), seeded[..1]);
    let cursor = page.body["page_info"]["next_cursor"]
        .as_str()
        .expect("cursor")
        .to_owned();
    let next = call(&router, discover(&format!("?$top=1&$skiptoken={cursor}"))).await;
    assert_eq!(page_ids(&next.body), seeded[1..2]);

    for (query, field) in [
        ("?limit=1&$top=1".to_owned(), "$top"),
        (
            format!("?cursor={cursor}&$skiptoken={cursor}"),
            "$skiptoken",
        ),
        ("?limit=1&limit=2".to_owned(), "limit"),
        ("?pattern=a&pattern=b".to_owned(), "pattern"),
    ] {
        let response = call(&router, discover(&query)).await;
        assert_field_refusal(&response, field, "VALIDATION_FAILED");
    }
    let zero = call(&router, discover("?$top=0")).await;
    assert_field_refusal(&zero, "$top", "VALIDATION_FAILED");
}

#[tokio::test]
async fn discovery_refuses_undeclared_and_unsupported_parameters() {
    let router = router_with_db().await;

    for (query, field) in [
        ("?$filter=gts_id%20eq%20'x'", "$filter"),
        ("?$orderby=gts_id", "$orderby"),
        ("?$skip=1", "$skip"),
        ("?$count=true", "$count"),
        ("?$expand=content", "$expand"),
        ("?$filtre=x", "$filtre"),
        ("?is_schema=true", "is_schema"),
        ("?vendor=cf", "vendor"),
        ("?package=core", "package"),
        ("?namespace=example", "namespace"),
        ("?segmentScope=any", "segmentScope"),
        ("?segment_scope=any", "segment_scope"),
    ] {
        let response = call(&router, discover(query)).await;
        assert_field_refusal(&response, field, "UNSUPPORTED_QUERY_PARAM");
    }

    for select in [
        "",
        "content,,kind",
        "availability",
        "content.title",
        "kind,KIND",
        "nope",
    ] {
        let response = call(&router, discover(&format!("?$select={select}"))).await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST, "{select:?}");
        assert_eq!(
            response.body["context"]["field_violations"][0]["field"],
            json!("$select"),
            "{select:?}: {:?}",
            response.body,
        );
    }
}

/// The pair ceiling counts raw pairs. No 32-pair discovery request is otherwise
/// valid (the vocabulary is smaller and repeats are refused), so the 32-pair case
/// proves only that it passes the count guard and reaches the parameter guard,
/// which names at most 16 unknown keys.
#[tokio::test]
async fn the_pair_ceiling_counts_raw_pairs_and_unknown_keys_are_reported_up_to_16() {
    let router = router_with_db().await;
    let unknown = |n: usize| {
        let pairs: Vec<String> = (0..n).map(|i| format!("x{i}=1")).collect();
        format!("?{}", pairs.join("&"))
    };

    let at = call(&router, discover(&unknown(32))).await;
    assert_eq!(at.status, StatusCode::BAD_REQUEST, "{:?}", at.body);
    let violations = at.body["context"]["field_violations"]
        .as_array()
        .expect("field_violations is an array");
    let fields: Vec<&str> = violations
        .iter()
        .map(|v| {
            assert_eq!(v["reason"], json!("UNSUPPORTED_QUERY_PARAM"), "{v}");
            v["field"].as_str().expect("a field name")
        })
        .collect();
    let first_16: Vec<String> = (0..16).map(|i| format!("x{i}")).collect();
    assert_eq!(fields, first_16, "32 pairs pass the count guard");

    let over = call(&router, discover(&unknown(33))).await;
    assert_field_refusal(&over, "query", "VALIDATION_FAILED");
    assert_eq!(
        over.body["context"]["field_violations"][0]["description"],
        json!("at most 32 query parameters are accepted; this request has 33"),
    );
}

/// Caller input echoed into a refusal is cut to its first 64 characters.
#[tokio::test]
async fn echoed_caller_input_is_cut_to_64_characters() {
    let router = router_with_db().await;
    let long = "k".repeat(2048);
    let shown = &long[..64];

    let kind = call(&router, discover(&format!("?kind={long}"))).await;
    assert_field_refusal(&kind, "kind", "VALIDATION_FAILED");
    assert_eq!(
        kind.body["context"]["field_violations"][0]["description"],
        json!(format!(
            "kind must be `type_schema` or `instance`, not `{shown}`"
        )),
    );

    let unknown = call(&router, discover(&format!("?{long}=1"))).await;
    assert_field_refusal(&unknown, shown, "UNSUPPORTED_QUERY_PARAM");
    let description = unknown.body["context"]["field_violations"][0]["description"]
        .as_str()
        .unwrap_or_default();
    assert!(
        description.starts_with(&format!("unsupported query parameter `{shown}`;")),
        "{description}"
    );
}

// ---------------------------------------------------------------------------
// Discovery by `kind` (T22c)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn discovery_narrows_by_kind_and_intersects_with_pattern_and_select() {
    let router = router_with_db().await;
    register_entity(&router, "arrange-other", CF_OTHER_TYPE).await;
    register_type_and_instance(&router).await;

    let schemas = call(&router, discover("?kind=type_schema")).await;
    assert_eq!(page_ids(&schemas.body), [CF_OTHER_TYPE, CF_TYPE]);
    let instances = call(&router, discover("?kind=instance")).await;
    assert_eq!(page_ids(&instances.body), [CF_INSTANCE]);
    let all = call(&router, discover("")).await;
    assert_eq!(page_ids(&all.body), [CF_OTHER_TYPE, CF_TYPE, CF_INSTANCE]);

    let pattern = format!("{}cf.core.example.type.v1~*", gts::GTS_ID_PREFIX);
    let narrowed = call(
        &router,
        discover(&format!("?kind=type_schema&pattern={pattern}")),
    )
    .await;
    assert_eq!(page_ids(&narrowed.body), [CF_TYPE]);

    let projected = call(&router, discover("?kind=instance&$select=gts_id,content")).await;
    assert_eq!(
        projected.body["items"],
        json!([{
            "gts_id": CF_INSTANCE,
            "gts_uuid": gts::GtsId::try_new(CF_INSTANCE).expect("id").to_uuid(),
            "kind": "instance",
            "lifecycle_status": "active",
            "content": { "name": "first" },
        }]),
    );
}

#[tokio::test]
async fn kind_filtering_excludes_tombstones() {
    let router = router_with_db().await;
    register_entity(&router, "arrange", CF_TYPE).await;
    let deleted = call(
        &router,
        delete_one(Some("delete"), CF_TYPE, "?expected_resource_version=1"),
    )
    .await;
    assert_eq!(deleted.status, StatusCode::ACCEPTED, "{:?}", deleted.body);
    let page = call(&router, discover("?kind=type_schema")).await;
    assert_eq!(page.status, StatusCode::OK, "{:?}", page.body);
    assert!(page_ids(&page.body).is_empty(), "{:?}", page.body);
}

/// A kind page over interleaved kinds still progresses to every match.
#[tokio::test]
async fn a_kind_traversal_visits_every_match_once() {
    let router = router_with_db().await;
    register_entity(&router, "arrange-other", CF_OTHER_TYPE).await;
    register_type_and_instance(&router).await;
    let items = traverse(&router, "?limit=1&kind=type_schema").await;
    let ids: Vec<&str> = items
        .iter()
        .map(|item| item["gts_id"].as_str().expect("default gts_id"))
        .collect();
    assert_eq!(ids, [CF_OTHER_TYPE, CF_TYPE]);
}

#[tokio::test]
async fn a_cursor_is_refused_under_a_different_kind() {
    let (router, db) = router_and_db().await;
    _ = seed_entities(&db, 3).await;

    for (issued, resumed) in [
        ("?limit=1", "?limit=1&kind=type_schema"),
        ("?limit=1&kind=type_schema", "?limit=1"),
        ("?limit=1&kind=type_schema", "?limit=1&kind=instance"),
    ] {
        let cursor = first_cursor(&router, issued).await;
        let response = call(&router, discover(&format!("{resumed}&cursor={cursor}"))).await;
        assert_field_refusal(&response, "cursor", "VALIDATION_FAILED");
    }
    let cursor = first_cursor(&router, "?limit=1&kind=type_schema").await;
    let resumed = call(
        &router,
        discover(&format!("?limit=1&kind=type_schema&cursor={cursor}")),
    )
    .await;
    assert_eq!(resumed.status, StatusCode::OK, "{:?}", resumed.body);
}

#[tokio::test]
async fn an_unknown_kind_is_refused_and_is_schema_is_not_an_alias() {
    let router = router_with_db().await;
    for value in ["type", "Type_Schema", "schema", "instances", "", "1"] {
        let response = call(&router, discover(&format!("?kind={value}"))).await;
        assert_field_refusal(&response, "kind", "VALIDATION_FAILED");
    }
    let repeated = call(&router, discover("?kind=instance&kind=type_schema")).await;
    assert_field_refusal(&repeated, "kind", "VALIDATION_FAILED");
    for query in [
        "?is_schema=true",
        "?is_schema=false",
        "?$filter=kind%20eq%20'instance'",
    ] {
        let response = call(&router, discover(query)).await;
        assert_eq!(
            response.status,
            StatusCode::BAD_REQUEST,
            "{query}: {:?}",
            response.body
        );
        assert_eq!(
            response.body["context"]["field_violations"][0]["reason"],
            json!("UNSUPPORTED_QUERY_PARAM"),
            "{query}",
        );
    }
}

/// The generated document, not the builder input: `ParamSpec` has no `enum` or
/// `default`, so `kind` and `lifecycle_status` are optional plain strings whose
/// vocabulary is in the description.
fn generated_openapi() -> Value {
    let registry = toolkit::api::OpenApiRegistryImpl::new();
    let config = TypesRegistryConfig::default();
    let legacy = Arc::new(TypesRegistryService::new(
        Arc::new(InMemoryGtsRepository::new(config.to_gts_config())),
        config,
    ));
    let _router =
        types_registry::api::rest::routes::register_routes(Router::new(), &registry, legacy, None);
    let document = registry
        .build_openapi(&toolkit::api::OpenApiInfo::default())
        .expect("the document builds");
    serde_json::to_value(document).expect("the document serializes")
}

#[tokio::test]
async fn a_malformed_lifecycle_status_is_refused() {
    let router = router_with_db().await;
    for value in [
        "",
        "ACTIVE",
        "Active",
        "any",
        "active,deleted",
        "%20active",
        "tombstone",
    ] {
        let response = call(&router, discover(&format!("?lifecycle_status={value}"))).await;
        assert_field_refusal(&response, "lifecycle_status", "VALIDATION_FAILED");
    }
    let repeated = call(
        &router,
        discover("?lifecycle_status=all&lifecycle_status=all"),
    )
    .await;
    assert_field_refusal(&repeated, "lifecycle_status", "VALIDATION_FAILED");
}

/// Omitted and explicit `active` are one binding; `deleted` and `all` are not.
#[tokio::test]
async fn a_cursor_binds_the_normalized_lifecycle_status() {
    let (router, db) = router_and_db().await;
    _ = seed_entities(&db, 3).await;

    for (issued, resumed) in [
        ("?limit=1", "?limit=1&lifecycle_status=deleted"),
        ("?limit=1", "?limit=1&lifecycle_status=all"),
        (
            "?limit=1&lifecycle_status=active",
            "?limit=1&lifecycle_status=all",
        ),
        ("?limit=1&lifecycle_status=all", "?limit=1"),
        (
            "?limit=1&lifecycle_status=all",
            "?limit=1&lifecycle_status=deleted",
        ),
    ] {
        let cursor = first_cursor(&router, issued).await;
        let response = call(&router, discover(&format!("{resumed}&cursor={cursor}"))).await;
        assert_field_refusal(&response, "cursor", "VALIDATION_FAILED");
    }
    for (issued, resumed) in [
        ("?limit=1", "?limit=1&lifecycle_status=active"),
        ("?limit=1&lifecycle_status=active", "?limit=1"),
        (
            "?limit=1&lifecycle_status=all",
            "?limit=1&lifecycle_status=all",
        ),
    ] {
        let cursor = first_cursor(&router, issued).await;
        let response = call(&router, discover(&format!("{resumed}&cursor={cursor}"))).await;
        assert_eq!(response.status, StatusCode::OK, "{issued} -> {resumed}");
        assert_eq!(page_ids(&response.body).len(), 1, "{issued} -> {resumed}");
    }
}

#[test]
fn discovery_declares_kind_and_lifecycle_status_as_optional_strings() {
    let doc = generated_openapi();
    let params = doc["paths"][format!("{V2}/entities")]["get"]["parameters"]
        .as_array()
        .expect("discovery parameters")
        .clone();
    for name in ["kind", "lifecycle_status"] {
        let param = params
            .iter()
            .find(|p| p["name"] == name)
            .unwrap_or_else(|| panic!("{name} is declared: {params:?}"));
        assert_eq!(param["in"], "query", "{name}");
        assert_eq!(param["required"], false, "{name}");
        assert_eq!(param["schema"], json!({ "type": "string" }), "{name}");
    }
    let lifecycle = params
        .iter()
        .find(|p| p["name"] == "lifecycle_status")
        .expect("declared");
    let text = lifecycle["description"].as_str().expect("described");
    for word in ["`active` (default)", "`deleted`", "`all`"] {
        assert!(text.contains(word), "{word}: {text}");
    }
}

/// All three database-backed reads answer with the one `EntityDto`, whose
/// identity, kind and lifecycle are required and never `null`.
#[test]
fn every_entity_read_requires_identity_kind_and_lifecycle_in_the_generated_document() {
    let doc = generated_openapi();
    let schemas = &doc["components"]["schemas"];
    let entity = &schemas["EntityDto"];
    assert_eq!(
        entity["required"],
        json!(["gts_id", "gts_uuid", "kind", "lifecycle_status"])
    );
    assert_eq!(entity["properties"]["gts_id"]["type"], "string");
    assert_eq!(
        (
            &entity["properties"]["gts_uuid"]["type"],
            &entity["properties"]["gts_uuid"]["format"],
        ),
        (&json!("string"), &json!("uuid"))
    );
    let kind = &entity["properties"]["kind"];
    assert_eq!(kind["$ref"], "#/components/schemas/EntityKindDto", "{kind}");
    assert!(kind.get("oneOf").is_none(), "never nullable: {kind}");
    let provenance = schemas["ProvenanceDto"]["properties"]
        .as_object()
        .expect("provenance properties");
    let mut members: Vec<&str> = provenance.keys().map(String::as_str).collect();
    members.sort_unstable();
    assert_eq!(
        members,
        ["compat_forced", "gts_impl_version", "gts_spec_version"],
        "attribution is not published in P0",
    );
    let body = |path: String, method: &str| {
        doc["paths"][path][method]["responses"]["200"]["content"]["application/json"]["schema"]
            ["$ref"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    };
    let entity_ref = "#/components/schemas/EntityDto";
    assert_eq!(
        body(format!("{V2}/entities/{{entity_key}}"), "get"),
        entity_ref
    );
    assert_eq!(
        body(format!("{V2}/entities:batchGet"), "post"),
        "#/components/schemas/EntityLookupsDto"
    );
    assert_eq!(
        body(format!("{V2}/entities"), "get"),
        "#/components/schemas/EntityPageDto"
    );
    let lookup = &schemas["EntityLookupDto"]["properties"]["entity"]["oneOf"];
    assert_eq!(lookup.as_array().map(Vec::len), Some(2), "{lookup}");
    assert_eq!(lookup[0], json!({"type": "null"}));
    assert_eq!(lookup[1]["$ref"], entity_ref);
    assert_eq!(
        schemas["EntityPageDto"]["properties"]["items"]["items"]["$ref"],
        entity_ref
    );
}

// ---------------------------------------------------------------------------
// Discovery by `depth` (T22c)
// ---------------------------------------------------------------------------

/// A Type Schema derived from [`CF_TYPE`], two segments deep.
const CF_DERIVED: &str = gts_id!("cf.core.example.type.v1~cf.core.example.derived.v1~");

async fn register_derived(router: &Router) {
    let mut derived = schema(CF_DERIVED);
    derived["allOf"] = json!([{ "$ref": format!("gts://{CF_TYPE}") }]);
    let accepted = call(
        router,
        submit(
            Some("arrange-derived"),
            &json!({ "items": [{ "gts_id": CF_DERIVED, "content": derived }] }),
        ),
    )
    .await;
    let operation = poll(router, &accepted).await;
    assert_eq!(
        operation["items"][0]["status"],
        json!("succeeded"),
        "{operation}"
    );
}

#[tokio::test]
async fn depth_is_an_inclusive_segment_maximum_on_roots_derived_schemas_and_instances() {
    let router = router_with_db().await;
    register_entity(&router, "arrange-other", CF_OTHER_TYPE).await;
    register_type_and_instance(&router).await;
    register_derived(&router).await;

    let everything = [CF_OTHER_TYPE, CF_TYPE, CF_INSTANCE, CF_DERIVED];
    for (query, want) in [
        ("?depth=1", &[CF_OTHER_TYPE, CF_TYPE][..]),
        ("?depth=2", &everything[..]),
        ("?depth=255", &everything[..]),
        ("", &everything[..]),
        ("?depth=1&kind=instance", &[][..]),
        ("?depth=2&kind=instance", &[CF_INSTANCE][..]),
        (
            "?depth=2&kind=type_schema",
            &[CF_OTHER_TYPE, CF_TYPE, CF_DERIVED][..],
        ),
    ] {
        let page = call(&router, discover(query)).await;
        assert_eq!(page.status, StatusCode::OK, "{query}: {:?}", page.body);
        let mut want: Vec<&str> = want.to_vec();
        want.sort_unstable();
        assert_eq!(page_ids(&page.body), want, "{query}");
    }

    let pattern = format!("{}cf.core.example.type.v1~*", gts::GTS_ID_PREFIX);
    for (depth, want) in [
        ("1", &[CF_TYPE][..]),
        ("2", &[CF_TYPE, CF_INSTANCE, CF_DERIVED][..]),
    ] {
        let query = format!("?pattern={pattern}&depth={depth}&$select=gts_id,kind");
        let page = call(&router, discover(&query)).await;
        let mut want: Vec<&str> = want.to_vec();
        want.sort_unstable();
        assert_eq!(page_ids(&page.body), want, "{query}");
        for item in page.body["items"].as_array().expect("items") {
            assert_eq!(
                field_names(item),
                ["gts_id", "gts_uuid", "kind", "lifecycle_status"]
            );
        }
    }
}

#[tokio::test]
async fn depth_filtering_excludes_tombstones() {
    let router = router_with_db().await;
    register_entity(&router, "arrange", CF_TYPE).await;
    let deleted = call(
        &router,
        delete_one(Some("delete"), CF_TYPE, "?expected_resource_version=1"),
    )
    .await;
    assert_eq!(deleted.status, StatusCode::ACCEPTED, "{:?}", deleted.body);
    let page = call(&router, discover("?depth=1")).await;
    assert!(page_ids(&page.body).is_empty(), "{:?}", page.body);
}

#[tokio::test]
async fn a_malformed_depth_is_refused() {
    let router = router_with_db().await;
    for value in [
        "0",
        "-1",
        "+1",
        "1.5",
        "1e2",
        "two",
        "",
        "256",
        "99999999999999999999",
        "%201",
    ] {
        let response = call(&router, discover(&format!("?depth={value}"))).await;
        assert_field_refusal(&response, "depth", "VALIDATION_FAILED");
    }
    let repeated = call(&router, discover("?depth=1&depth=2")).await;
    assert_field_refusal(&repeated, "depth", "VALIDATION_FAILED");
}

#[tokio::test]
async fn a_cursor_is_refused_under_a_different_depth() {
    let (router, db) = router_and_db().await;
    _ = seed_entities(&db, 3).await;

    for (issued, resumed) in [
        ("?limit=1", "?limit=1&depth=1"),
        ("?limit=1&depth=1", "?limit=1"),
        ("?limit=1&depth=1", "?limit=1&depth=2"),
        ("?limit=1&depth=1&kind=type_schema", "?limit=1&depth=1"),
    ] {
        let cursor = first_cursor(&router, issued).await;
        let response = call(&router, discover(&format!("{resumed}&cursor={cursor}"))).await;
        assert_field_refusal(&response, "cursor", "VALIDATION_FAILED");
    }
    let cursor = first_cursor(&router, "?limit=1&depth=1&kind=type_schema").await;
    let resumed = call(
        &router,
        discover(&format!(
            "?limit=1&kind=type_schema&depth=1&cursor={cursor}"
        )),
    )
    .await;
    assert_eq!(resumed.status, StatusCode::OK, "{:?}", resumed.body);
}

/// A depth-1 row, thousands of depth-2 rows, then another depth-1 row: each
/// `limit=1` page holds one match, and the second page has no cursor.
#[tokio::test]
async fn a_sparse_depth_traversal_returns_full_pages() {
    let (router, db) = router_and_db().await;
    let near = format!("{}cf.core.example.aaa.v1~", gts::GTS_ID_PREFIX);
    let far = format!("{}cf.core.example.zzz.v1~", gts::GTS_ID_PREFIX);
    let gap: Vec<String> = (0..2_100)
        .map(|i| {
            format!(
                "{}cf.core.example.bbb.v1~cf.core.example.n{i:05}.v1~",
                gts::GTS_ID_PREFIX
            )
        })
        .collect();
    let mut ids: Vec<&str> = gap.iter().map(String::as_str).collect();
    ids.push(&near);
    ids.push(&far);
    seed_ids(&db, &ids).await;

    for base in ["?limit=1&depth=1", "?limit=1&depth=1&lifecycle_status=all"] {
        let first = call(&router, discover(base)).await;
        assert_eq!(first.status, StatusCode::OK, "{:?}", first.body);
        assert_eq!(page_ids(&first.body), [near.as_str()], "{base}");
        let cursor = first.body["page_info"]["next_cursor"]
            .as_str()
            .expect("a cursor while a match remains");
        let second = call(&router, discover(&format!("{base}&cursor={cursor}"))).await;
        assert_eq!(second.status, StatusCode::OK, "{:?}", second.body);
        assert_eq!(page_ids(&second.body), [far.as_str()], "{base}");
        assert!(
            second.body["page_info"]["next_cursor"].is_null(),
            "{base}: no cursor after the last match"
        );
    }
}

/// `ToolKit`'s extractor parses `$select` and the `CursorV1` token in one request:
/// its limits apply, and a continuation under a respelled `$select` resumes.
#[tokio::test]
async fn toolkit_select_extraction_and_the_v1_cursor_work_together() {
    let (router, db) = router_and_db().await;
    _ = seed_entities(&db, 3).await;

    let too_long = format!("gts_id,{}", "x".repeat(2_048));
    let too_many = vec!["kind"; 101].join(",");
    for (select, description) in [
        (too_long.as_str(), "$select too long"),
        (too_many.as_str(), "$select contains too many fields"),
    ] {
        let response = call(&router, discover(&format!("?$select={select}"))).await;
        assert_eq!(
            response.status,
            StatusCode::BAD_REQUEST,
            "{:?}",
            response.body
        );
        assert_eq!(
            response.body["context"]["field_violations"][0],
            json!({
                "field": "$select",
                "reason": "INVALID_SELECT",
                "description": description,
            }),
        );
    }

    let cursor = first_cursor(&router, "?limit=1&$select=GTS_ID,%20content").await;
    assert!(
        toolkit_odata::CursorV1::decode(&cursor).is_ok(),
        "discovery issues ToolKit's own cursor: {cursor}"
    );
    let resumed = call(
        &router,
        discover(&format!(
            "?limit=1&$select=content,gts_id&$skiptoken={cursor}"
        )),
    )
    .await;
    assert_eq!(resumed.status, StatusCode::OK, "{:?}", resumed.body);
    assert_eq!(
        field_names(&resumed.body["items"][0]),
        ["content", "gts_id", "gts_uuid", "kind", "lifecycle_status"]
    );
}

#[test]
fn discovery_declares_depth_as_a_positive_integer() {
    let openapi = TestOpenApi::default();
    let _router = router_for_openapi(&openapi);
    let params = openapi.params.lock().expect("params lock");
    let (_, declared) = params
        .iter()
        .find(|(id, _)| id == "types_registry.list_entities")
        .expect("discovery registered");
    let depth = declared
        .iter()
        .find(|p| p.0 == "depth")
        .expect("depth is declared");
    assert_eq!(depth.1, ParamLocation::Query);
    assert!(!depth.2);
    assert_eq!(depth.3, "integer");
    assert_eq!(depth.5, Some(1.0));
}

/// `limit=0` is refused, so neither the request nor the page reports a zero size.
/// The maximum is configured, so neither side declares one.
#[test]
fn discovery_declares_the_page_size_as_positive_on_request_and_page() {
    let doc = generated_openapi();
    let params = doc["paths"][format!("{V2}/entities")]["get"]["parameters"]
        .as_array()
        .expect("discovery parameters")
        .clone();
    let limit = params
        .iter()
        .find(|p| p["name"] == "limit")
        .expect("limit is declared");
    assert_eq!(limit["schema"], json!({ "type": "integer", "minimum": 1 }));
    let applied = &doc["components"]["schemas"]["PageInfoDto"]["properties"]["limit"];
    assert_eq!(applied["minimum"], 1, "{applied}");
    assert!(applied.get("maximum").is_none(), "{applied}");
}
