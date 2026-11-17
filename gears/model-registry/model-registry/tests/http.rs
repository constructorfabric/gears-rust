//! The REST surface through real requests (DESIGN §3.3, §4 Testability):
//! status codes, `Location` and `ETag`, the `If-Match` rules, body and path
//! rejections as canonical Problems, `OData` options, and the REST-vs-SDK
//! contract of every operation.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cognitive_complexity)]

use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use axum::{Extension, Router};
use model_registry::api::rest::routes::register_routes;
use model_registry::config::ModelRegistryConfig;
use model_registry::domain::access_guard::AccessGuard;
use model_registry::domain::clock::ManualClock;
use model_registry::domain::error::DomainError;
use model_registry::domain::local_client::LocalClient;
use model_registry::domain::service::Service;
use model_registry::infra::chain_cache::ChainCache;
use model_registry::infra::storage::migrations::Migrator;
use model_registry::testing::{
    StaticAuthz, StaticSchemaSource, StaticSecretRefSource, StaticTenantResolver, model_update,
    provider_update, security_context,
};
use model_registry_sdk::gts::{MODEL_RESOURCE_TYPE, PROVIDER_RESOURCE_TYPE};
use model_registry_sdk::{
    ContextWindow, CreateModelRequestV1, CreateProviderRequestV1, ModelRegistryClientV1, field,
    reason::{aborted, permission},
};
use sea_orm_migration::MigratorTrait;
use serde_json::{Value, json};
use toolkit::api::operation_builder::OperationSpec;
use toolkit::api::{OpenApiRegistry, canonical_error_middleware};
use toolkit_canonical_errors::{CanonicalError, Problem};
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use toolkit_gts::gts_id;
use toolkit_odata::ODataQuery;
use tower::ServiceExt;
use uuid::Uuid;

const OPENAI: &str = gts_id!("cf.genai.model.provider.v1~cf.genai._.openai.v1~");
const STRICT: &str = gts_id!("cf.genai.model.provider.v1~cf.genai._.strict.v1~");
const BASE: &str = "/model-registry/v1";

struct NoopRegistry;

impl OpenApiRegistry for NoopRegistry {
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

/// Root → Child, both served by one service over one in-memory database.
struct Harness {
    service: Arc<Service>,
    sdk: LocalClient,
    root: Uuid,
    child: Uuid,
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}

impl Reply {
    fn header(&self, name: header::HeaderName) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    /// `(field, reason)` of every field violation, sorted.
    fn violations(&self) -> Vec<(String, String)> {
        let mut found: Vec<(String, String)> = self.body["context"]["field_violations"]
            .as_array()
            .unwrap_or_else(|| panic!("no field violations in {}", self.body))
            .iter()
            .map(|v| {
                (
                    v["field"].as_str().unwrap().to_owned(),
                    v["reason"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        found.sort();
        found
    }
}

async fn harness_with(authz: StaticAuthz) -> Harness {
    let raw = connect_db(
        "sqlite::memory:",
        ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("in-memory SQLite");
    run_migrations_for_testing(&raw, Migrator::migrations())
        .await
        .expect("migrations");
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let cfg = ModelRegistryConfig::default();
    let clock = Arc::new(ManualClock::fixed());
    let chain_cache = Arc::new(ChainCache::new(
        Arc::new(
            StaticTenantResolver::new()
                .with_chain(root, vec![])
                .with_chain(child, vec![root]),
        ),
        clock.clone(),
        cfg.chain_cache_ttl(),
        cfg.chain_cache_max_entries,
        cfg.tenant_resolver_deadline(),
    ));
    let guard = AccessGuard::new(
        PolicyEnforcer::new(Arc::new(authz)).with_deadline(cfg.pdp_deadline()),
        cfg.max_pdp_constraint_values,
    );
    let schemas = StaticSchemaSource::new().with_open_type(OPENAI).with_type(
        STRICT,
        &json!({
            "type": "object",
            "required": ["oagw_alias"],
            "properties": { "oagw_alias": { "type": "string" } },
            "additionalProperties": false
        }),
    );
    let service = Arc::new(Service::new(
        Arc::new(DBProvider::<DomainError>::new(raw)),
        chain_cache,
        guard,
        Arc::new(schemas),
        Arc::new(StaticSecretRefSource::new().owned("openai-key")),
        clock,
        &cfg,
    ));
    Harness {
        service: service.clone(),
        sdk: LocalClient::new(service),
        root,
        child,
    }
}

async fn harness() -> Harness {
    harness_with(StaticAuthz::permit_own_tenant()).await
}

impl Harness {
    /// The gear's routes as the platform serves them to `tenant`.
    fn app(&self, tenant: Uuid) -> Router {
        register_routes(Router::new(), &NoopRegistry, self.service.clone())
            .layer(axum::middleware::from_fn(canonical_error_middleware))
            .layer(Extension(security_context(tenant)))
    }

    async fn call(
        &self,
        tenant: Uuid,
        method: Method,
        path: &str,
        if_match: Option<&str>,
        body: Option<&Value>,
    ) -> Reply {
        let mut req = Request::builder()
            .method(method)
            .uri(format!("{BASE}{path}"));
        if let Some(tag) = if_match {
            req = req.header(header::IF_MATCH, tag);
        }
        let req = match body {
            Some(json) => req
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(json).unwrap())),
            None => req.body(Body::empty()),
        }
        .unwrap();
        self.send(tenant, req).await
    }

    async fn send(&self, tenant: Uuid, req: Request<Body>) -> Reply {
        let response = self.app(tenant).oneshot(req).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
        };
        Reply {
            status,
            headers,
            body,
        }
    }

    async fn get(&self, tenant: Uuid, path: &str) -> Reply {
        self.call(tenant, Method::GET, path, None, None).await
    }

    async fn post(&self, tenant: Uuid, path: &str, body: &Value) -> Reply {
        self.call(tenant, Method::POST, path, None, Some(body))
            .await
    }

    async fn patch(&self, tenant: Uuid, path: &str, if_match: Option<&str>, body: &Value) -> Reply {
        self.call(tenant, Method::PATCH, path, if_match, Some(body))
            .await
    }

    async fn delete(&self, tenant: Uuid, path: &str, if_match: Option<&str>) -> Reply {
        self.call(tenant, Method::DELETE, path, if_match, None)
            .await
    }

    /// A provider created over REST; returns its id.
    async fn provider(&self, tenant: Uuid, slug: &str, sharing_mode: &str) -> String {
        let reply = self
            .post(
                tenant,
                "/providers",
                &json!({ "slug": slug, "name": slug, "gts_type": OPENAI, "sharing_mode": sharing_mode }),
            )
            .await;
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
        reply.body["id"].as_str().unwrap().to_owned()
    }

    /// A model created over REST; returns its id.
    async fn model(&self, tenant: Uuid, provider: &str, name: &str, extra: Value) -> String {
        let mut body = json!({
            "provider_id": provider,
            "name": name,
            "provider_model_id": name,
            "display_name": name,
            "context_window": { "max_input_tokens": 8 },
            "sharing_mode": "shared"
        });
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().cloned().unwrap_or_default());
        let reply = self.post(tenant, "/models", &body).await;
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
        reply.body["id"].as_str().unwrap().to_owned()
    }
}

fn sdk_json<T: serde::Serialize>(record: &T) -> Value {
    serde_json::to_value(record).unwrap()
}

/// The paths at which two JSON values differ.
fn json_diff(rest: &Value, sdk: &Value, path: &str, out: &mut Vec<String>) {
    match (rest, sdk) {
        (Value::Object(a), Value::Object(b)) => {
            let keys: std::collections::BTreeSet<&String> = a.keys().chain(b.keys()).collect();
            for key in keys {
                let at = format!("{path}/{key}");
                match (a.get(key), b.get(key)) {
                    (Some(x), Some(y)) => json_diff(x, y, &at, out),
                    (x, y) => out.push(format!("{at}: rest {x:?}, sdk {y:?}")),
                }
            }
        }
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
            for (i, (x, y)) in a.iter().zip(b).enumerate() {
                json_diff(x, y, &format!("{path}/{i}"), out);
            }
        }
        (x, y) if x == y => {}
        (x, y) => out.push(format!("{path}: rest {x}, sdk {y}")),
    }
}

/// The same record on both surfaces, absent optional fields included.
fn assert_same_json(rest: &Value, sdk: &Value, what: &str) {
    let mut diff = Vec::new();
    json_diff(rest, sdk, "", &mut diff);
    assert!(diff.is_empty(), "{what} differs:\n{}", diff.join("\n"));
}

fn problem(err: CanonicalError) -> Value {
    serde_json::to_value(Problem::from(err)).unwrap()
}

/// The REST reply carries the Problem the SDK error renders to: same status,
/// type, and context.
fn assert_same_problem(rest: &Reply, sdk: CanonicalError, what: &str) {
    let expected = problem(sdk);
    assert_eq!(rest.status.as_u16(), expected["status"], "{what}: status");
    assert_eq!(rest.body["type"], expected["type"], "{what}: type");
    assert_eq!(rest.body["context"], expected["context"], "{what}: context");
}

// ---------------------------------------------------------------------------
// Providers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn provider_lifecycle_statuses_headers_and_preconditions() {
    let h = harness().await;
    let created = h
        .post(
            h.root,
            "/providers",
            &json!({ "slug": "openai", "name": "OpenAI", "gts_type": OPENAI }),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED);
    let id = created.body["id"].as_str().unwrap().to_owned();
    assert_eq!(
        created.header(header::LOCATION),
        Some(format!("{BASE}/providers/{id}").as_str())
    );
    assert_eq!(created.header(header::ETAG), Some("\"1\""));
    // The entry is flattened: provider fields and flags at the top level.
    assert_eq!(created.body["slug"], "openai");
    assert_eq!(created.body["sharing_mode"], "tenant");
    assert_eq!(created.body["enabled"], true);
    assert_eq!(created.body["version"], 1);
    assert_eq!(created.body["inherited"], false);
    assert_eq!(created.body["shadowed"], false);
    assert_eq!(created.body["available_for_eval"], true);

    let path = format!("/providers/{id}");
    let got = h.get(h.root, &path).await;
    assert_eq!(got.status, StatusCode::OK);
    assert_eq!(got.header(header::ETAG), Some("\"1\""));
    assert_eq!(got.body, created.body);

    // `If-Match` is required on PATCH.
    let missing = h.patch(h.root, &path, None, &json!({ "name": "x" })).await;
    assert_eq!(missing.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        missing.violations(),
        vec![("If-Match".to_owned(), field::IF_MATCH_REQUIRED.to_owned())]
    );
    assert_eq!(
        missing.body["context"]["resource_type"],
        PROVIDER_RESOURCE_TYPE
    );

    // The validator is compared with the ETag as a string: a weak or
    // differently spelled one never matches.
    for tag in ["W/\"1\"", "\"01\"", "\"+1\"", "1"] {
        let unmatched = h
            .patch(h.root, &path, Some(tag), &json!({ "name": "x" }))
            .await;
        assert_eq!(unmatched.status, StatusCode::CONFLICT, "{tag}");
        assert_eq!(
            unmatched.body["context"]["reason"],
            aborted::OPTIMISTIC_LOCK_FAILURE
        );
    }

    // A stale validator is `aborted` (409), not 412, and changes nothing.
    let stale = h
        .patch(h.root, &path, Some("\"9\""), &json!({ "name": "x" }))
        .await;
    assert_eq!(stale.status, StatusCode::CONFLICT);
    assert_eq!(
        stale.body["context"]["reason"],
        aborted::OPTIMISTIC_LOCK_FAILURE
    );
    assert_eq!(
        stale.body["context"]["resource_type"],
        PROVIDER_RESOURCE_TYPE
    );

    let updated = h
        .patch(
            h.root,
            &path,
            Some("\"1\""),
            &json!({ "name": "OpenAI 2", "enabled": false, "metadata": { "k": 1 } }),
        )
        .await;
    assert_eq!(updated.status, StatusCode::OK, "{}", updated.body);
    assert_eq!(updated.header(header::ETAG), Some("\"2\""));
    assert_eq!(updated.body["name"], "OpenAI 2");
    assert_eq!(updated.body["enabled"], false);
    assert_eq!(updated.body["available_for_eval"], false);
    assert_eq!(updated.body["metadata"], json!({ "k": 1 }));

    // `*` is an unconditional update; `null` clears an optional field.
    let cleared = h
        .patch(h.root, &path, Some("*"), &json!({ "metadata": null }))
        .await;
    assert_eq!(cleared.status, StatusCode::OK);
    assert_eq!(cleared.header(header::ETAG), Some("\"3\""));
    assert!(cleared.body["metadata"].is_null());
    assert_eq!(cleared.body["name"], "OpenAI 2", "absent fields stay");

    // Immutable fields and `null` on a required field are rejected together.
    let rejected = h
        .patch(
            h.root,
            &path,
            Some("*"),
            &json!({ "slug": "other", "version": 7, "name": null }),
        )
        .await;
    assert_eq!(rejected.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        rejected.violations(),
        vec![
            ("name".to_owned(), field::INVALID_FORMAT.to_owned()),
            ("slug".to_owned(), field::IMMUTABLE_FIELD.to_owned()),
            ("version".to_owned(), field::IMMUTABLE_FIELD.to_owned()),
        ]
    );
    assert_eq!(
        rejected.body["context"]["resource_type"],
        PROVIDER_RESOURCE_TYPE
    );

    // An unknown key is rejected, not dropped into an empty update.
    let unknown = h
        .patch(h.root, &path, Some("*"), &json!({ "nmae": "typo" }))
        .await;
    assert_eq!(unknown.status, StatusCode::BAD_REQUEST, "{}", unknown.body);
    assert_eq!(
        unknown.violations(),
        vec![("body".to_owned(), field::INVALID_FORMAT.to_owned())]
    );
    assert_eq!(
        h.get(h.root, &path).await.header(header::ETAG),
        Some("\"3\"")
    );
    let unknown = h
        .post(
            h.root,
            "/providers",
            &json!({ "slug": "x", "name": "X", "gts_type": OPENAI, "enabeld": false }),
        )
        .await;
    assert_eq!(
        unknown.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        unknown.body
    );

    let duplicate = h
        .post(
            h.root,
            "/providers",
            &json!({ "slug": "openai", "name": "Again", "gts_type": OPENAI }),
        )
        .await;
    assert_eq!(duplicate.status, StatusCode::CONFLICT);
    assert_eq!(duplicate.body["context"]["resource_name"], "openai");

    // DELETE takes an optional `If-Match`.
    let stale = h.delete(h.root, &path, Some("\"1\"")).await;
    assert_eq!(stale.status, StatusCode::CONFLICT);
    let deleted = h.delete(h.root, &path, Some("\"3\"")).await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    assert!(deleted.body.is_null());
    let gone = h.get(h.root, &path).await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND);
    assert_eq!(
        gone.body["context"]["resource_type"],
        PROVIDER_RESOURCE_TYPE
    );
    assert_eq!(gone.body["context"]["resource_name"], id);
    assert_eq!(
        h.delete(h.root, &path, None).await.status,
        StatusCode::NOT_FOUND
    );
}

/// Behind a gateway `prefix_path`, `Location` keeps the prefix the client used.
#[tokio::test]
async fn location_keeps_the_gateway_prefix() {
    let h = harness().await;
    let app = Router::new().nest("/cf", h.app(h.root));
    let req = Request::builder()
        .method(Method::POST)
        .uri(format!("/cf{BASE}/providers"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&json!({ "slug": "openai", "name": "OpenAI", "gts_type": OPENAI }))
                .unwrap(),
        ))
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let location = response.headers()[header::LOCATION].to_str().unwrap();
    assert!(
        location.starts_with(&format!("/cf{BASE}/providers/")),
        "{location}"
    );
}

#[tokio::test]
async fn provider_type_migration_over_patch() {
    let h = harness().await;
    let provider = h.provider(h.root, "openai", "shared").await;
    let ok = h
        .model(
            h.root,
            &provider,
            "gpt-4o",
            json!({ "provider_settings": { "oagw_alias": "openai" } }),
        )
        .await;
    let path = format!("/providers/{provider}");

    let moved = h
        .patch(h.root, &path, Some("\"1\""), &json!({ "gts_type": STRICT }))
        .await;
    assert_eq!(moved.status, StatusCode::OK, "{}", moved.body);
    assert_eq!(moved.body["gts_type"], STRICT);
    assert_eq!(moved.body["id"], provider.as_str());
    let model = h.get(h.root, &format!("/models/{ok}")).await;
    assert_eq!(model.body["info"]["gts_type"], STRICT);
    assert_eq!(model.body["canonical_id"], "openai::gpt-4o");
    assert_eq!(model.body["version"], 2);

    // One model that fails the new schema fails the migration whole.
    h.patch(h.root, &path, Some("*"), &json!({ "gts_type": OPENAI }))
        .await;
    h.model(
        h.root,
        &provider,
        "loose",
        json!({ "provider_settings": { "anything": true } }),
    )
    .await;
    let refused = h
        .patch(h.root, &path, Some("*"), &json!({ "gts_type": STRICT }))
        .await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);
    assert!(
        refused
            .violations()
            .iter()
            .all(|(f, r)| f == "gts_type" && r == field::SCHEMA_MISMATCH),
        "{}",
        refused.body
    );
    assert_eq!(h.get(h.root, &path).await.body["gts_type"], OPENAI);
}

// ---------------------------------------------------------------------------
// Models and resolution
// ---------------------------------------------------------------------------

#[tokio::test]
async fn model_lifecycle_and_resolution() {
    let h = harness().await;
    let provider = h.provider(h.root, "openai", "shared").await;
    let created = h
        .post(
            h.root,
            "/models",
            &json!({
                "provider_id": provider,
                "name": "gpt-4o",
                "provider_model_id": "gpt-4o-2026",
                "display_name": "GPT-4o",
                "context_window": { "max_input_tokens": 128_000, "max_output_tokens": 4096 },
                "sharing_mode": "shared",
                "base_url": "https://openai.example/v1",
                "secret_ref": "openai-key",
                "supported_api": ["completion"],
                "capabilities": { "streaming": true },
                "cost": { "input_per_1m_micro": 5 },
                "provider_settings": { "oagw_alias": "openai" }
            }),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let id = created.body["id"].as_str().unwrap().to_owned();
    assert_eq!(
        created.header(header::LOCATION),
        Some(format!("{BASE}/models/{id}").as_str())
    );
    assert_eq!(created.header(header::ETAG), Some("\"1\""));
    assert_eq!(created.body["canonical_id"], "openai::gpt-4o");
    assert_eq!(created.body["approval"]["state"], "approved");
    assert_eq!(created.body["info"]["gts_type"], OPENAI);
    assert_eq!(created.body["info"]["provider_model_id"], "gpt-4o-2026");
    assert_eq!(created.body["info"]["cost"]["input_per_1m_micro"], 5);
    assert_eq!(created.body["provider_enabled"], true);
    assert_eq!(created.body["available_for_eval"], true);

    let path = format!("/models/{id}");
    let got = h.get(h.root, &path).await;
    assert_eq!(got.status, StatusCode::OK);
    assert_eq!(got.header(header::ETAG), Some("\"1\""));
    assert_eq!(got.body, created.body);

    // The canonical id is one percent-encoded path segment; a descendant
    // gets the same full record.
    for tenant in [h.root, h.child] {
        for segment in ["openai%3A%3Agpt-4o", "openai::gpt-4o"] {
            let resolved = h.get(tenant, &format!("/available-models/{segment}")).await;
            assert_eq!(resolved.status, StatusCode::OK, "{}", resolved.body);
            assert_eq!(resolved.body["id"], id.as_str());
            assert_eq!(
                resolved.body["info"]["provider_settings"],
                json!({ "oagw_alias": "openai" })
            );
            assert_eq!(resolved.body["info"]["secret_ref"], "openai-key");
            assert!(resolved.body.get("inherited").is_none(), "no entry flags");
        }
    }
    // `%2F` is decoded once and stays inside the segment.
    let slash = h.get(h.root, "/available-models/openai%3A%3Aa%2Fb").await;
    assert_eq!(slash.status, StatusCode::NOT_FOUND);
    assert_eq!(slash.body["context"]["resource_type"], MODEL_RESOURCE_TYPE);
    assert_eq!(slash.body["context"]["resource_name"], "openai::a/b");

    // `null` clears an optional field; a required field cannot be `null`.
    let cleared = h
        .patch(
            h.root,
            &path,
            Some("\"1\""),
            &json!({ "base_url": null, "display_name": "GPT-4o (new)" }),
        )
        .await;
    assert_eq!(cleared.status, StatusCode::OK, "{}", cleared.body);
    assert_eq!(cleared.header(header::ETAG), Some("\"2\""));
    assert!(cleared.body["info"]["base_url"].is_null());
    assert_eq!(cleared.body["info"]["display_name"], "GPT-4o (new)");
    assert_eq!(cleared.body["info"]["secret_ref"], "openai-key");

    let rejected = h
        .patch(
            h.root,
            &path,
            Some("*"),
            &json!({ "provider_settings": null, "name": "x", "gts_type": STRICT }),
        )
        .await;
    assert_eq!(rejected.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        rejected.violations(),
        vec![
            ("gts_type".to_owned(), field::IMMUTABLE_FIELD.to_owned()),
            ("name".to_owned(), field::IMMUTABLE_FIELD.to_owned()),
            (
                "provider_settings".to_owned(),
                field::INVALID_FORMAT.to_owned()
            ),
        ]
    );
    assert_eq!(
        rejected.body["context"]["resource_type"],
        MODEL_RESOURCE_TYPE
    );

    let missing = h
        .patch(h.root, &path, None, &json!({ "sort_order": 1 }))
        .await;
    assert_eq!(
        missing.violations(),
        vec![("If-Match".to_owned(), field::IF_MATCH_REQUIRED.to_owned())]
    );

    // A revoke stops resolution on the next call; a stale client cannot
    // re-approve.
    let revoked = h
        .patch(
            h.root,
            &path,
            Some("\"2\""),
            &json!({ "approval_state": "revoked" }),
        )
        .await;
    assert_eq!(revoked.status, StatusCode::OK);
    assert_eq!(revoked.body["approval"]["state"], "revoked");
    assert_eq!(revoked.body["available_for_eval"], false);
    let refused = h.get(h.child, "/available-models/openai%3A%3Agpt-4o").await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert_eq!(
        refused.body["context"]["reason"],
        permission::MODEL_NOT_APPROVED
    );
    let stale = h
        .patch(
            h.root,
            &path,
            Some("\"2\""),
            &json!({ "approval_state": "approved" }),
        )
        .await;
    assert_eq!(stale.status, StatusCode::CONFLICT);
    assert_eq!(
        stale.body["context"]["reason"],
        aborted::OPTIMISTIC_LOCK_FAILURE
    );
    assert_eq!(
        h.get(h.root, &path).await.body["approval"]["state"],
        "revoked"
    );

    // Inherited entries are read-only for the descendant.
    let inherited = h.get(h.child, &path).await;
    assert_eq!(inherited.status, StatusCode::OK);
    assert_eq!(inherited.body["inherited"], true);
    for reply in [
        h.patch(h.child, &path, Some("*"), &json!({ "sort_order": 1 }))
            .await,
        h.delete(h.child, &path, None).await,
    ] {
        assert_eq!(reply.status, StatusCode::FORBIDDEN);
        assert_eq!(
            reply.body["context"]["reason"],
            permission::INHERITED_READ_ONLY
        );
    }

    assert_eq!(
        h.delete(h.root, &path, Some("\"1\"")).await.status,
        StatusCode::CONFLICT
    );
    assert_eq!(
        h.delete(h.root, &path, None).await.status,
        StatusCode::NO_CONTENT
    );
    let gone = h.get(h.root, &path).await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND);
    assert_eq!(gone.body["context"]["resource_type"], MODEL_RESOURCE_TYPE);
}

#[tokio::test]
async fn model_write_validation_reaches_the_wire() {
    let h = harness().await;
    let provider = h.provider(h.root, "strict", "tenant").await;
    h.patch(
        h.root,
        &format!("/providers/{provider}"),
        Some("*"),
        &json!({ "gts_type": STRICT }),
    )
    .await;
    let body = |settings: Value, secret: &str| {
        json!({
            "provider_id": provider,
            "name": "m",
            "provider_model_id": "m",
            "display_name": "M",
            "context_window": { "max_input_tokens": 8 },
            "provider_settings": settings,
            "secret_ref": secret
        })
    };

    let mismatch = h
        .post(
            h.root,
            "/models",
            &body(json!({ "oagw_alias": 7, "extra": 1 }), "openai-key"),
        )
        .await;
    assert_eq!(mismatch.status, StatusCode::BAD_REQUEST);
    let violations = mismatch.violations();
    assert!(!violations.is_empty());
    assert!(
        violations
            .iter()
            .all(|(f, r)| f.starts_with("provider_settings") && r == field::SCHEMA_MISMATCH),
        "{}",
        mismatch.body
    );

    let unknown_secret = h
        .post(
            h.root,
            "/models",
            &body(json!({ "oagw_alias": "a" }), "no-such-key"),
        )
        .await;
    assert_eq!(
        unknown_secret.violations(),
        vec![(
            "secret_ref".to_owned(),
            field::SECRET_REF_NOT_FOUND.to_owned()
        )]
    );

    let mut bad_enum = body(json!({ "oagw_alias": "a" }), "openai-key");
    bad_enum["sharing_mode"] = json!("subtree");
    let reply = h.post(h.root, "/models", &bad_enum).await;
    assert_eq!(
        reply.violations(),
        vec![("sharing_mode".to_owned(), field::INVALID_FORMAT.to_owned())]
    );

    let mut unknown_provider = body(json!({ "oagw_alias": "a" }), "openai-key");
    unknown_provider["provider_id"] = json!(Uuid::new_v4());
    let reply = h.post(h.root, "/models", &unknown_provider).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(
        reply.body["context"]["resource_type"],
        PROVIDER_RESOURCE_TYPE
    );
}

// ---------------------------------------------------------------------------
// Body and path rejections
// ---------------------------------------------------------------------------

#[tokio::test]
async fn malformed_bodies_and_paths_are_canonical_problems() {
    let h = harness().await;
    let provider = h.provider(h.root, "openai", "tenant").await;

    let invalid_argument = |reply: &Reply, what: &str| {
        assert!(
            reply.status.is_client_error(),
            "{what}: {} {}",
            reply.status,
            reply.body
        );
        assert!(
            reply.body["type"]
                .as_str()
                .is_some_and(|t| t.contains("invalid_argument")),
            "{what}: {}",
            reply.body
        );
        assert_eq!(
            reply.header(header::CONTENT_TYPE),
            Some("application/problem+json"),
            "{what}"
        );
    };

    let missing_field = h.post(h.root, "/providers", &json!({ "slug": "x" })).await;
    invalid_argument(&missing_field, "POST without required fields");

    let wrong_type = h
        .post(
            h.root,
            "/models",
            &json!({ "provider_id": 7, "name": "m", "provider_model_id": "m", "display_name": "M",
                     "context_window": { "max_input_tokens": 8 } }),
        )
        .await;
    invalid_argument(&wrong_type, "POST with a wrong field type");

    let syntax = Request::builder()
        .method(Method::POST)
        .uri(format!("{BASE}/providers"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{ not json"))
        .unwrap();
    invalid_argument(&h.send(h.root, syntax).await, "POST with broken JSON");

    let not_an_object = h
        .patch(
            h.root,
            &format!("/providers/{provider}"),
            Some("*"),
            &json!(["name"]),
        )
        .await;
    assert_eq!(
        not_an_object.violations(),
        vec![("body".to_owned(), field::INVALID_FORMAT.to_owned())]
    );

    let wrong_patch_type = h
        .patch(
            h.root,
            &format!("/providers/{provider}"),
            Some("*"),
            &json!({ "enabled": "yes" }),
        )
        .await;
    assert_eq!(
        wrong_patch_type.violations(),
        vec![("body".to_owned(), field::INVALID_FORMAT.to_owned())]
    );

    for (method, path) in [
        (Method::GET, "/providers/not-a-uuid"),
        (Method::DELETE, "/providers/not-a-uuid"),
        (Method::GET, "/models/not-a-uuid"),
        (Method::DELETE, "/models/not-a-uuid"),
    ] {
        let reply = h.call(h.root, method.clone(), path, None, None).await;
        invalid_argument(&reply, &format!("{method} {path}"));
    }
}

// ---------------------------------------------------------------------------
// Lists
// ---------------------------------------------------------------------------

#[tokio::test]
async fn lists_page_filter_order_and_project() {
    let h = harness().await;
    let provider = h.provider(h.root, "openai", "shared").await;
    h.provider(h.root, "anthropic", "tenant").await;
    for (name, extra) in [
        (
            "a-vision",
            json!({ "capabilities": { "vision": { "enabled": true } }, "family": "gpt" }),
        ),
        ("b-plain", json!({ "sort_order": 5 })),
        ("c-pending", json!({ "approval_state": "pending" })),
    ] {
        h.model(h.root, &provider, name, extra).await;
    }
    let names = |reply: &Reply, key: &str| -> Vec<String> {
        reply.body["items"]
            .as_array()
            .unwrap_or_else(|| panic!("no items in {}", reply.body))
            .iter()
            .map(|i| i[key].as_str().unwrap_or_default().to_owned())
            .collect()
    };

    // Default order and page shape.
    let providers = h.get(h.root, "/providers").await;
    assert_eq!(providers.status, StatusCode::OK);
    assert_eq!(names(&providers, "slug"), ["anthropic", "openai"]);
    assert_eq!(providers.body["page_info"]["limit"], 20);
    assert!(
        providers.header(header::ETAG).is_none(),
        "lists carry no ETag"
    );

    // The admin list holds every state; the available list only what resolves.
    let admin = h.get(h.root, "/models").await;
    assert_eq!(
        names(&admin, "canonical_id"),
        ["openai::a-vision", "openai::b-plain", "openai::c-pending"]
    );
    assert_eq!(admin.body["items"][2]["available_for_eval"], false);
    let available = h.get(h.child, "/available-models").await;
    assert_eq!(
        names(&available, "canonical_id"),
        ["openai::a-vision", "openai::b-plain"]
    );
    assert!(available.body["items"][0].get("inherited").is_none());
    assert!(available.body["items"][0]["info"]["provider_settings"].is_object());

    // `$filter` on promoted columns, ANDed onto the visibility predicates.
    let vision = h
        .get(h.child, "/available-models?$filter=vision%20eq%20true")
        .await;
    assert_eq!(names(&vision, "canonical_id"), ["openai::a-vision"]);
    let widen = h
        .get(
            h.child,
            "/available-models?$filter=approval_state%20eq%20%27pending%27",
        )
        .await;
    assert!(names(&widen, "canonical_id").is_empty());
    let shared = h
        .get(
            h.child,
            "/providers?$filter=sharing_mode%20eq%20%27tenant%27",
        )
        .await;
    assert!(
        names(&shared, "slug").is_empty(),
        "ancestor tenant-mode rows"
    );

    // `$orderby`, `limit`, and the keyset cursor.
    let first = h
        .get(h.root, "/models?$orderby=sort_order%20desc&limit=1")
        .await;
    assert_eq!(names(&first, "canonical_id"), ["openai::b-plain"]);
    assert_eq!(first.body["page_info"]["limit"], 1);
    let cursor = first.body["page_info"]["next_cursor"]
        .as_str()
        .expect("next cursor")
        .to_owned();
    let second = h
        .get(h.root, &format!("/models?limit=1&cursor={cursor}"))
        .await;
    assert_eq!(second.status, StatusCode::OK, "{}", second.body);
    assert_eq!(names(&second, "canonical_id").len(), 1);
    assert_ne!(names(&second, "canonical_id"), ["openai::b-plain"]);
    let capped = h.get(h.root, "/models?limit=1000").await;
    assert_eq!(capped.body["page_info"]["limit"], 100);

    // `$select` projects the serialized items.
    let projected = h.get(h.root, "/providers?$select=slug,enabled").await;
    assert_eq!(projected.status, StatusCode::OK, "{}", projected.body);
    for item in projected.body["items"].as_array().unwrap() {
        let mut keys: Vec<&String> = item.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(keys, ["enabled", "slug"], "{item}");
    }

    // Invalid options are `invalid_argument`.
    for query in [
        "/models?$orderby=family%20asc",
        "/models?$filter=no_such_field%20eq%201",
        "/models?$filter=vision%20eq",
        "/models?cursor=garbage",
        "/providers?$orderby=nope",
    ] {
        let reply = h.get(h.root, query).await;
        assert_eq!(
            reply.status,
            StatusCode::BAD_REQUEST,
            "{query}: {}",
            reply.body
        );
        assert!(
            reply.body["type"]
                .as_str()
                .is_some_and(|t| t.contains("invalid_argument")),
            "{query}: {}",
            reply.body
        );
    }
}

// ---------------------------------------------------------------------------
// REST-vs-SDK contract
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rest_and_sdk_return_the_same_records() {
    let h = harness().await;
    let provider = h.provider(h.root, "openai", "shared").await;
    let model = h
        .model(
            h.root,
            &provider,
            "gpt-4o",
            json!({ "base_url": "https://openai.example/v1", "family": "gpt" }),
        )
        .await;
    // A second model and an own provider, so the lists carry several items.
    h.model(
        h.root,
        &provider,
        "gpt-4o-mini",
        json!({ "vendor": "openai" }),
    )
    .await;
    h.provider(h.child, "anthropic", "tenant").await;
    let ctx = security_context(h.child);
    let provider_id = Uuid::parse_str(&provider).unwrap();
    let model_id = Uuid::parse_str(&model).unwrap();

    // The resolution routes return the SDK record itself.
    let sdk = h.sdk.resolve_model(&ctx, "openai::gpt-4o").await.unwrap();
    let rest = h.get(h.child, "/available-models/openai%3A%3Agpt-4o").await;
    assert_same_json(&rest.body, &sdk_json(&sdk), "resolve_model");
    let sdk = h
        .sdk
        .list_available_models(&ctx, &ODataQuery::default())
        .await
        .unwrap();
    let rest = h.get(h.child, "/available-models").await;
    assert_eq!(sdk.items.len(), 2);
    assert_same_json(&rest.body, &sdk_json(&sdk), "list_available_models");

    // The management routes flatten the SDK entry: the record plus the flags.
    let flatten = |record: Value, flags: &[(&str, bool)]| {
        let mut flat = record;
        for (name, value) in flags {
            flat[*name] = json!(value);
        }
        flat
    };
    let sdk = h.sdk.get_model(&ctx, model_id).await.unwrap();
    let rest = h.get(h.child, &format!("/models/{model}")).await;
    assert_same_json(
        &rest.body,
        &flatten(
            sdk_json(&sdk.model),
            &[
                ("inherited", sdk.inherited),
                ("shadowed", sdk.shadowed),
                ("provider_enabled", sdk.provider_enabled),
                ("available_for_eval", sdk.available_for_eval),
            ],
        ),
        "get_model",
    );
    assert!(sdk.inherited);
    let sdk = h.sdk.get_provider(&ctx, provider_id).await.unwrap();
    let rest = h.get(h.child, &format!("/providers/{provider}")).await;
    assert_same_json(
        &rest.body,
        &flatten(
            sdk_json(&sdk.provider),
            &[
                ("inherited", sdk.inherited),
                ("shadowed", sdk.shadowed),
                ("available_for_eval", sdk.available_for_eval),
            ],
        ),
        "get_provider",
    );
    let sdk = h
        .sdk
        .list_models(&ctx, &ODataQuery::default())
        .await
        .unwrap();
    let rest = h.get(h.child, "/models").await;
    assert_eq!(sdk.items.len(), 2);
    let items: Vec<Value> = sdk
        .items
        .iter()
        .map(|e| {
            flatten(
                sdk_json(&e.model),
                &[
                    ("inherited", e.inherited),
                    ("shadowed", e.shadowed),
                    ("provider_enabled", e.provider_enabled),
                    ("available_for_eval", e.available_for_eval),
                ],
            )
        })
        .collect();
    assert_same_json(
        &rest.body,
        &json!({ "items": items, "page_info": sdk_json(&sdk.page_info) }),
        "list_models",
    );
    let sdk = h
        .sdk
        .list_providers(&ctx, &ODataQuery::default())
        .await
        .unwrap();
    let rest = h.get(h.child, "/providers").await;
    assert_eq!(sdk.items.len(), 2);
    let items: Vec<Value> = sdk
        .items
        .iter()
        .map(|e| {
            flatten(
                sdk_json(&e.provider),
                &[
                    ("inherited", e.inherited),
                    ("shadowed", e.shadowed),
                    ("available_for_eval", e.available_for_eval),
                ],
            )
        })
        .collect();
    assert_same_json(
        &rest.body,
        &json!({ "items": items, "page_info": sdk_json(&sdk.page_info) }),
        "list_providers",
    );
}

#[tokio::test]
async fn rest_and_sdk_report_the_same_errors() {
    let h = harness().await;
    let provider = h.provider(h.root, "openai", "shared").await;
    let model = h.model(h.root, &provider, "gpt-4o", json!({})).await;
    h.model(
        h.root,
        &provider,
        "pending",
        json!({ "approval_state": "pending" }),
    )
    .await;
    let off = h.provider(h.root, "off", "shared").await;
    h.model(h.root, &off, "m", json!({})).await;
    h.patch(
        h.root,
        &format!("/providers/{off}"),
        Some("*"),
        &json!({ "enabled": false }),
    )
    .await;
    let root = security_context(h.root);
    let child = security_context(h.child);
    let provider_id = Uuid::parse_str(&provider).unwrap();
    let model_id = Uuid::parse_str(&model).unwrap();
    let unknown = Uuid::new_v4();

    // Resolution: not found, provider disabled, not approved.
    for canonical_id in ["openai::nope", "no-separator", "off::m", "openai::pending"] {
        let sdk = h.sdk.resolve_model(&child, canonical_id).await.unwrap_err();
        let rest = h
            .get(
                h.child,
                &format!("/available-models/{}", canonical_id.replace(':', "%3A")),
            )
            .await;
        assert_same_problem(&rest, sdk, canonical_id);
    }

    // Point reads of an unknown id.
    let sdk = h.sdk.get_model(&root, unknown).await.unwrap_err();
    let rest = h.get(h.root, &format!("/models/{unknown}")).await;
    assert_same_problem(&rest, sdk, "get_model");
    let sdk = h.sdk.get_provider(&root, unknown).await.unwrap_err();
    let rest = h.get(h.root, &format!("/providers/{unknown}")).await;
    assert_same_problem(&rest, sdk, "get_provider");

    // Duplicates.
    let sdk = h
        .sdk
        .create_provider(
            &root,
            CreateProviderRequestV1::builder("openai", "x", gts::GtsTypeId::new(OPENAI)).build(),
        )
        .await
        .unwrap_err();
    let rest = h
        .post(
            h.root,
            "/providers",
            &json!({ "slug": "openai", "name": "x", "gts_type": OPENAI }),
        )
        .await;
    assert_same_problem(&rest, sdk, "create_provider duplicate");
    let sdk = h
        .sdk
        .create_model(
            &root,
            CreateModelRequestV1::new(provider_id, "gpt-4o", "x", "x", ContextWindow::new(8)),
        )
        .await
        .unwrap_err();
    let rest = h
        .post(
            h.root,
            "/models",
            &json!({ "provider_id": provider, "name": "gpt-4o", "provider_model_id": "x",
                     "display_name": "x", "context_window": { "max_input_tokens": 8 } }),
        )
        .await;
    assert_same_problem(&rest, sdk, "create_model duplicate");

    // Field validation.
    let sdk = h
        .sdk
        .create_provider(
            &root,
            CreateProviderRequestV1::builder("Bad Slug", "", gts::GtsTypeId::new("nope")).build(),
        )
        .await
        .unwrap_err();
    let rest = h
        .post(
            h.root,
            "/providers",
            &json!({ "slug": "Bad Slug", "name": "", "gts_type": "nope" }),
        )
        .await;
    assert_same_problem(&rest, sdk, "create_provider validation");

    // Stale precondition.
    let sdk = h
        .sdk
        .update_model(
            &root,
            model_id,
            model_update(|r| {
                r.expected_version = Some(9);
                r.sort_order = Some(1);
            }),
        )
        .await
        .unwrap_err();
    let rest = h
        .patch(
            h.root,
            &format!("/models/{model}"),
            Some("\"9\""),
            &json!({ "sort_order": 1 }),
        )
        .await;
    assert_same_problem(&rest, sdk, "update_model stale");
    let sdk = h
        .sdk
        .delete_provider(&root, provider_id, Some(9))
        .await
        .unwrap_err();
    let rest = h
        .delete(h.root, &format!("/providers/{provider}"), Some("\"9\""))
        .await;
    assert_same_problem(&rest, sdk, "delete_provider stale");

    // Writes on inherited entries.
    let sdk = h
        .sdk
        .update_provider(
            &child,
            provider_id,
            provider_update(|r| r.name = Some("x".into())),
        )
        .await
        .unwrap_err();
    let rest = h
        .patch(
            h.child,
            &format!("/providers/{provider}"),
            Some("*"),
            &json!({ "name": "x" }),
        )
        .await;
    assert_same_problem(&rest, sdk, "update_provider inherited");
    let sdk = h
        .sdk
        .delete_model(&child, model_id, None)
        .await
        .unwrap_err();
    let rest = h.delete(h.child, &format!("/models/{model}"), None).await;
    assert_same_problem(&rest, sdk, "delete_model inherited");
    let sdk = h
        .sdk
        .create_model(
            &child,
            CreateModelRequestV1::new(provider_id, "mine", "x", "x", ContextWindow::new(8)),
        )
        .await
        .unwrap_err();
    let rest = h
        .post(
            h.child,
            "/models",
            &json!({ "provider_id": provider, "name": "mine", "provider_model_id": "x",
                     "display_name": "x", "context_window": { "max_input_tokens": 8 } }),
        )
        .await;
    assert_same_problem(&rest, sdk, "create_model under an inherited provider");
    assert_eq!(rest.status, StatusCode::FORBIDDEN);
}

/// Method, path, body, and the resource type of the expected Problem.
type Route<'a> = (Method, String, Option<&'a Value>, &'a str);

#[tokio::test]
async fn a_pdp_denial_is_not_permitted_on_every_route() {
    let h = harness_with(StaticAuthz::deny_all()).await;
    let id = Uuid::new_v4();
    let provider_body = json!({ "slug": "openai", "name": "x", "gts_type": OPENAI });
    let model_body = json!({ "provider_id": id, "name": "m", "provider_model_id": "m",
                             "display_name": "M", "context_window": { "max_input_tokens": 8 } });
    let patch = json!({ "managed": true });
    let calls: [Route<'_>; 11] = [
        (
            Method::GET,
            "/available-models".into(),
            None,
            MODEL_RESOURCE_TYPE,
        ),
        (
            Method::POST,
            "/providers".into(),
            Some(&provider_body),
            PROVIDER_RESOURCE_TYPE,
        ),
        (
            Method::GET,
            format!("/providers/{id}"),
            None,
            PROVIDER_RESOURCE_TYPE,
        ),
        (
            Method::GET,
            "/providers".into(),
            None,
            PROVIDER_RESOURCE_TYPE,
        ),
        (
            Method::PATCH,
            format!("/providers/{id}"),
            Some(&patch),
            PROVIDER_RESOURCE_TYPE,
        ),
        (
            Method::DELETE,
            format!("/providers/{id}"),
            None,
            PROVIDER_RESOURCE_TYPE,
        ),
        (
            Method::POST,
            "/models".into(),
            Some(&model_body),
            MODEL_RESOURCE_TYPE,
        ),
        (
            Method::GET,
            format!("/models/{id}"),
            None,
            MODEL_RESOURCE_TYPE,
        ),
        (Method::GET, "/models".into(), None, MODEL_RESOURCE_TYPE),
        (
            Method::PATCH,
            format!("/models/{id}"),
            Some(&patch),
            MODEL_RESOURCE_TYPE,
        ),
        (
            Method::DELETE,
            format!("/models/{id}"),
            None,
            MODEL_RESOURCE_TYPE,
        ),
    ];
    for (method, path, body, resource_type) in calls {
        let if_match = (method == Method::PATCH).then_some("*");
        let reply = h.call(h.root, method.clone(), &path, if_match, body).await;
        let what = format!("{method} {path}");
        assert_eq!(
            reply.status,
            StatusCode::FORBIDDEN,
            "{what}: {}",
            reply.body
        );
        assert_eq!(
            reply.body["context"]["reason"],
            permission::NOT_PERMITTED,
            "{what}"
        );
        assert_eq!(
            reply.body["context"]["resource_type"], resource_type,
            "{what}"
        );
    }
    // Resolution reports catalog reasons before it asks the PDP.
    let reply = h.get(h.root, "/available-models/openai%3A%3Agpt-4o").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
}
