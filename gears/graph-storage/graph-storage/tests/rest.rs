#![allow(clippy::expect_used, clippy::unwrap_used)]

//! The REST surface, through the router the gear actually registers.
//!
//! The route test in `api::rest::routes` asserts that every operation
//! registers its schemas; nothing asserted that a request reaching one comes
//! back as the documented document. These cases send real HTTP through the
//! real router -- the handlers, the DTO conversions and the canonical error
//! mapping included -- with the platform's authentication layer stood in for
//! by an injected `SecurityContext`, which is what that layer produces.

#[allow(dead_code)]
mod conformance;
mod support;

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use graph_storage::api::rest::routes::register_routes;
use support::Harness;
use tower::ServiceExt;

/// Registers nothing: what is under test here is the traffic, not the
/// schemas (`every_route_registers_its_schemas` covers those).
struct NoopRegistry;

impl toolkit::api::OpenApiRegistry for NoopRegistry {
    fn register_operation(&self, _spec: &toolkit::api::OperationSpec) {}

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

const BASE: &str = "/graph-storage/v1";

struct Stand {
    router: Router,
    harness: Harness,
}

impl Stand {
    fn new(harness: Harness) -> Self {
        let router = register_routes(Router::new(), &NoopRegistry, Arc::clone(&harness.services))
            // What the platform's authentication middleware would have put
            // there. Injected rather than mocked at the transport level: the
            // handlers take it as an extension, and that is the seam.
            .layer(axum::Extension(harness.ctx()));
        Self { router, harness }
    }

    async fn call(&self, request: Request<Body>) -> (StatusCode, serde_json::Value) {
        let response = self
            .router
            .clone()
            .oneshot(request)
            .await
            .expect("the router answers");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .expect("the body reads");
        let body = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
        };
        (status, body)
    }

    async fn get(&self, path: &str) -> (StatusCode, serde_json::Value) {
        self.call(
            Request::builder()
                .uri(format!("{BASE}{path}"))
                .body(Body::empty())
                .expect("a valid request"),
        )
        .await
    }

    async fn post(&self, path: &str, body: &serde_json::Value) -> (StatusCode, serde_json::Value) {
        self.call(
            Request::builder()
                .method("POST")
                .uri(format!("{BASE}{path}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("a valid request"),
        )
        .await
    }

    async fn delete(&self, path: &str) -> (StatusCode, serde_json::Value) {
        self.call(
            Request::builder()
                .method("DELETE")
                .uri(format!("{BASE}{path}"))
                .body(Body::empty())
                .expect("a valid request"),
        )
        .await
    }
}

/// Register the ontology over HTTP, which is also the first assertion: the
/// gear's own base types go in through the same endpoint a producer uses.
async fn seed(stand: &Stand) {
    let types: Vec<serde_json::Value> = conformance::ontology_batch()
        .into_iter()
        .map(|registration| {
            serde_json::json!({
                "type_id": registration.type_id,
                "schema": registration.schema,
            })
        })
        .collect();
    let (status, body) = stand
        .post("/types", &serde_json::json!({ "types": types }))
        .await;
    assert_eq!(status, StatusCode::OK, "registration failed: {body}");
}

fn ingest_body() -> serde_json::Value {
    serde_json::json!({
        "nodes": [
            {"node_key": "rest-a", "type_id": conformance::OWNED, "name": "findable alpha"},
            {"node_key": "rest-b", "type_id": conformance::OWNED, "name": "findable beta"}
        ],
        "edges": [
            {"type_id": conformance::LINK, "src_node_key": "rest-a", "dst_node_key": "rest-b"}
        ]
    })
}

#[tokio::test]
async fn the_write_and_read_endpoints_answer_the_documented_documents() {
    let stand = Stand::new(Harness::allowed());
    seed(&stand).await;

    let (status, body) = stand.post("/ingest", &ingest_body()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["counts"]["nodes_inserted"], 2, "{body}");
    assert_eq!(body["counts"]["edges_inserted"], 1, "{body}");

    // Node read: payload, adjacency and the envelope, in one document.
    let (status, node) = stand.get("/nodes/rest-a").await;
    assert_eq!(status, StatusCode::OK, "{node}");
    assert_eq!(node["node_key"], "rest-a");
    assert_eq!(
        node["adjacency"].as_array().map(Vec::len),
        Some(1),
        "{node}"
    );
    assert_eq!(node["envelope"]["key"], "rest-a", "{node}");
    assert!(
        node["envelope"]["graph_revision"]["revision"].as_i64() > Some(0),
        "the envelope carries the observed revision: {node}"
    );

    // Edge read, addressed by the key the node read handed out.
    let edge_key = node["adjacency"][0]["edge_key"]
        .as_str()
        .expect("the adjacency entry names its edge")
        .to_owned();
    let (status, edge) = stand.get(&format!("/edges/{edge_key}")).await;
    assert_eq!(status, StatusCode::OK, "{edge}");
    assert_eq!(edge["src"], "rest-a");
    assert_eq!(edge["dst"], "rest-b");
    assert_eq!(edge["envelope"]["key"], edge_key, "{edge}");

    // Projection.
    let (status, page) = stand.get("/nodes?limit=10").await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["items"].as_array().map(Vec::len), Some(2), "{page}");

    // Search.
    let (status, hits) = stand
        .post(
            "/search",
            &serde_json::json!({"mode": "lexical", "query": "findable", "limit": 10}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{hits}");
    assert_eq!(hits["hits"].as_array().map(Vec::len), Some(2), "{hits}");

    // Traversal and neighborhood.
    let (status, walked) = stand
        .post(
            "/graph/traverse",
            &serde_json::json!({"seeds": ["rest-a"], "depth": 1}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{walked}");
    assert_eq!(
        walked["nodes"].as_array().map(Vec::len),
        Some(2),
        "{walked}"
    );

    let (status, around) = stand
        .post(
            "/graph/neighborhood",
            &serde_json::json!({"root": "rest-a", "depth": 1}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{around}");

    // Ontology reads.
    let (status, types) = stand.get("/types").await;
    assert_eq!(status, StatusCode::OK, "{types}");
    let (status, one) = stand.get(&format!("/types/{}", conformance::OWNED)).await;
    assert_eq!(status, StatusCode::OK, "{one}");
    assert_eq!(one["type_id"], conformance::OWNED);

    // Deletes, and the tombstone the next read observes.
    let (status, deleted) = stand.delete("/nodes/rest-a").await;
    assert_eq!(status, StatusCode::OK, "{deleted}");
    assert_eq!(deleted["tombstoned_nodes"], 1, "{deleted}");
    let (status, _) = stand.get("/nodes/rest-a").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "a tombstoned node is gone");
}

#[tokio::test]
async fn readiness_is_reachable_without_a_caller_and_names_its_components() {
    let stand = Stand::new(Harness::allowed());
    let (status, body) = stand.get("/health/ready").await;
    assert!(
        status == StatusCode::OK || status == StatusCode::SERVICE_UNAVAILABLE,
        "readiness answers one way or the other, got {status}: {body}"
    );
    assert!(
        body["components"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty()),
        "the matrix's rows are on the answer: {body}"
    );
}

#[tokio::test]
async fn the_namespace_endpoints_list_and_transfer() {
    let stand = Stand::new(Harness::allowed());
    seed(&stand).await;

    let (status, listed) = stand.get("/source-namespaces").await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(listed["items"].as_array().map(Vec::len), Some(0));

    let (status, moved) = stand
        .post(
            "/source-namespaces/mirror/owner",
            &serde_json::json!({"owner_principal": "mirror-gear"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    assert_eq!(moved["owner_principal"], "mirror-gear");

    let (_, listed) = stand.get("/source-namespaces").await;
    assert_eq!(
        listed["items"].as_array().map(Vec::len),
        Some(1),
        "{listed}"
    );
}

#[tokio::test]
async fn a_dry_run_compatibility_check_answers_without_writing() {
    let stand = Stand::new(Harness::allowed());
    seed(&stand).await;

    let changed = serde_json::json!({
        "type_id": conformance::OWNED,
        "schema": {
            "$id": format!("gts://{}", conformance::OWNED),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "x-gts-traits": {
                "full_text_search": ["/name"],
                "vector_search": ["/payload/summary"]
            },
            "type": "object",
            "allOf": [
                { "$ref": "gts://gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~" },
                { "type": "object", "properties": {
                    "payload": { "type": "object", "properties": {
                        "summary": { "type": "string" }
                    }}
                }}
            ]
        }
    });
    let (status, verdicts) = stand
        .post(
            "/types/compatibility",
            &serde_json::json!({ "types": [changed] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");
    let verdict = verdicts["items"]
        .as_array()
        .and_then(|rows| rows.first().cloned())
        .expect("one verdict per submitted type");
    assert!(
        verdict["change"].is_object(),
        "the verdict says what the change is: {verdict}"
    );
}

#[tokio::test]
async fn a_refusal_comes_back_as_a_problem_document_with_its_reason() {
    let stand = Stand::new(Harness::allowed());
    seed(&stand).await;

    // A bound, refused rather than clamped: `out_of_range`.
    let over = stand.harness.services.config().node_read_max_adjacency + 1;
    let (status, problem) = stand
        .get(&format!("/nodes/rest-a?adjacency_limit={over}"))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{problem}");
    assert!(
        problem["detail"].is_string() || problem["title"].is_string(),
        "the refusal is an RFC-9457 problem document: {problem}"
    );

    // A search mode that needs text and was given none: an argument error,
    // which is a different reason from a breached bound.
    let (status, problem) = stand
        .post("/search", &serde_json::json!({"mode": "hybrid"}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{problem}");

    // An unknown node: absent, never "denied", so an unauthorized key and a
    // nonexistent one are indistinguishable from outside.
    let (status, _) = stand.get("/nodes/never-existed").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// A denied caller gets the *absent* answer, not a forbidden one.
///
/// That is the documented posture and not an accident of mapping: telling a
/// caller "you may not see this" confirms the thing exists, so denied and
/// nonexistent answer identically (DESIGN, anti-enumeration). The case pins
/// it because the obvious "fix" is to return 403 and it would be a leak.
#[tokio::test]
async fn a_denied_caller_cannot_tell_denial_from_absence() {
    let stand = Stand::new(Harness::denied());
    for (status, body) in [
        stand.post("/ingest", &ingest_body()).await,
        stand.get("/nodes/rest-a").await,
        stand.get("/nodes").await,
    ] {
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    }
}

#[tokio::test]
async fn the_remaining_endpoints_answer_too() {
    let stand = Stand::new(Harness::allowed());
    seed(&stand).await;
    let (status, _) = stand.post("/ingest", &ingest_body()).await;
    assert_eq!(status, StatusCode::OK);

    // The revision surface.
    let (status, revision) = stand.get("/revision").await;
    assert_eq!(status, StatusCode::OK, "{revision}");
    assert!(revision["revision"].as_i64() > Some(0), "{revision}");

    // Delete one edge, leaving both its endpoints.
    let (_, node) = stand.get("/nodes/rest-a").await;
    let edge_key = node["adjacency"][0]["edge_key"]
        .as_str()
        .expect("the edge key")
        .to_owned();
    let (status, deleted) = stand.delete(&format!("/edges/{edge_key}")).await;
    assert_eq!(status, StatusCode::OK, "{deleted}");
    assert_eq!(deleted["tombstoned_edges"], 1, "{deleted}");
    let (status, _) = stand.get("/nodes/rest-b").await;
    assert_eq!(status, StatusCode::OK, "the endpoints outlive the edge");

    // Types, narrowed by kind, and one type by identifier.
    let (status, page) = stand.get("/types?kind=node").await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(
        page["items"]
            .as_array()
            .is_some_and(|rows| rows.iter().all(|row| row["kind"] == "node")),
        "the filter is applied, not ignored: {page}"
    );
}

/// Registering a changed schema under a registered identifier: refused by
/// default, admitted with `on_existing: update` when it is backward
/// compatible. The two calls differ only in the option, which is the point.
#[tokio::test]
async fn an_existing_identifier_is_a_conflict_by_default_and_updatable_on_request() {
    let stand = Stand::new(Harness::allowed());
    seed(&stand).await;

    let widened = serde_json::json!({
        "type_id": conformance::OWNED,
        "schema": {
            "$id": format!("gts://{}", conformance::OWNED),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "x-gts-traits": {
                "full_text_search": ["/name"],
                "vector_search": ["/payload/summary"]
            },
            "type": "object",
            "allOf": [
                { "$ref": "gts://gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~" },
                { "type": "object", "properties": {
                    "payload": { "type": "object", "properties": {
                        "summary": { "type": "string" }
                    }}
                }}
            ]
        }
    });

    let (status, refused) = stand
        .post("/types", &serde_json::json!({ "types": [widened] }))
        .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a different schema under a registered identifier is a conflict: {refused}"
    );

    // `update` alone is not enough either, and for a reason worth pinning:
    // the payload is an open model, so declaring `summary` *narrows* what it
    // accepts (gts section 4.4) and the schemas cannot prove the change safe.
    // What admits it is `revalidate` -- a claim about the stored rows, made
    // by reading them.
    let (status, still_refused) = stand
        .post(
            "/types",
            &serde_json::json!({
                "types": [widened],
                "options": { "on_existing": "update" }
            }),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "an open model cannot prove this one: {still_refused}"
    );

    let (status, updated) = stand
        .post(
            "/types",
            &serde_json::json!({
                "types": [widened],
                "options": { "on_existing": "update", "revalidate": true }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated[0]["outcome"], "updated", "{updated}");
    assert_eq!(
        updated[0]["admission_basis"], "data_backed",
        "the answer says on what ground it was admitted: {updated}"
    );
    assert_eq!(
        updated[0]["rows_validated"], 0,
        "and how many rows that ground rests on: {updated}"
    );
}
