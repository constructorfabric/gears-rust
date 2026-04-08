use oagw::test_support::{AppHarness, format_upstream_gts};
use uuid::Uuid;

// 7.8: POST upstream with valid body -> 201 + GTS id + alias generated.
#[tokio::test]
async fn create_upstream_success() {
    let h = AppHarness::builder().build().await;

    let resp = h
        .api_v1()
        .post_upstream()
        .with_body(serde_json::json!({
            "server": {
                "endpoints": [{"host": "api.openai.com", "port": 443, "scheme": "https"}]
            },
            "protocol": "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
            "enabled": true,
            "tags": []
        }))
        .expect_status(201)
        .await;

    let json = resp.json();
    let id_str = json["id"].as_str().unwrap();
    assert!(id_str.starts_with("gts.x.core.oagw.upstream.v1~"));
    assert_eq!(json["alias"].as_str().unwrap(), "api.openai.com");
}

// 7.8: POST with missing server -> 422 (serde deserialization error).
#[tokio::test]
async fn create_upstream_missing_server_returns_422() {
    let h = AppHarness::builder().build().await;

    h.api_v1()
        .post_upstream()
        .with_body(serde_json::json!({
            "protocol": "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1"
        }))
        .expect_status(422)
        .await;
}

// 7.9: GET upstream by GTS id -> 200.
#[tokio::test]
async fn get_upstream_by_gts_id() {
    let h = AppHarness::builder().build().await;

    let upstream = h
        .facade()
        .create_upstream(
            h.security_context().clone(),
            oagw_sdk::CreateUpstreamRequest::builder(
                oagw_sdk::Server {
                    endpoints: vec![oagw_sdk::Endpoint {
                        scheme: oagw_sdk::Scheme::Https,
                        host: "api.openai.com".into(),
                        port: 443,
                    }],
                },
                "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
            )
            .build(),
        )
        .await
        .unwrap();

    let gts_id = format_upstream_gts(upstream.id);
    let resp = h.api_v1().get_upstream(&gts_id).expect_status(200).await;
    let json = resp.json();
    assert_eq!(json["alias"].as_str().unwrap(), "api.openai.com");
}

// 7.9: GET with invalid GTS format -> 400.
#[tokio::test]
async fn get_upstream_invalid_gts_returns_400() {
    let h = AppHarness::builder().build().await;

    let resp = h
        .api_v1()
        .get_upstream("not-a-gts-id")
        .expect_status(400)
        .await;

    let json = resp.json();
    assert_eq!(
        json["type"].as_str().unwrap(),
        "gts.x.core.errors.err.v1~x.oagw.validation.error.v1"
    );
}

// 7.9: GET nonexistent -> 404.
#[tokio::test]
async fn get_upstream_nonexistent_returns_404() {
    let h = AppHarness::builder().build().await;
    let fake_id = format_upstream_gts(Uuid::new_v4());

    h.api_v1().get_upstream(&fake_id).expect_status(404).await;
}

// 7.10: PUT upstream -> 200 with updated fields, id unchanged.
#[tokio::test]
async fn update_upstream_preserves_id() {
    let h = AppHarness::builder().build().await;

    let upstream = h
        .facade()
        .create_upstream(
            h.security_context().clone(),
            oagw_sdk::CreateUpstreamRequest::builder(
                oagw_sdk::Server {
                    endpoints: vec![oagw_sdk::Endpoint {
                        scheme: oagw_sdk::Scheme::Https,
                        host: "10.0.0.1".into(),
                        port: 443,
                    }],
                },
                "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
            )
            .alias("openai")
            .build(),
        )
        .await
        .unwrap();

    let gts_id = format_upstream_gts(upstream.id);

    let resp = h
        .api_v1()
        .patch_upstream(&gts_id)
        .with_body(serde_json::json!({"alias": "openai-v2"}))
        .expect_status(200)
        .await;

    let json = resp.json();
    assert_eq!(json["id"].as_str().unwrap(), gts_id);
    assert_eq!(json["alias"].as_str().unwrap(), "openai-v2");
}

// 7.10: DELETE upstream -> 204 + routes cascade deleted.
#[tokio::test]
async fn delete_upstream_returns_204() {
    let h = AppHarness::builder().build().await;

    let upstream = h
        .facade()
        .create_upstream(
            h.security_context().clone(),
            oagw_sdk::CreateUpstreamRequest::builder(
                oagw_sdk::Server {
                    endpoints: vec![oagw_sdk::Endpoint {
                        scheme: oagw_sdk::Scheme::Https,
                        host: "api.openai.com".into(),
                        port: 443,
                    }],
                },
                "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
            )
            .build(),
        )
        .await
        .unwrap();

    let gts_id = format_upstream_gts(upstream.id);

    h.api_v1().delete_upstream(&gts_id).expect_status(204).await;
}

// 7.11: POST route -> 201 referencing existing upstream.
#[tokio::test]
async fn create_route_success() {
    let h = AppHarness::builder().build().await;

    let upstream = h
        .facade()
        .create_upstream(
            h.security_context().clone(),
            oagw_sdk::CreateUpstreamRequest::builder(
                oagw_sdk::Server {
                    endpoints: vec![oagw_sdk::Endpoint {
                        scheme: oagw_sdk::Scheme::Https,
                        host: "api.openai.com".into(),
                        port: 443,
                    }],
                },
                "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
            )
            .build(),
        )
        .await
        .unwrap();

    let resp = h
        .api_v1()
        .post_route()
        .with_body(serde_json::json!({
            "upstream_id": upstream.id,
            "match": {
                "http": {
                    "methods": ["POST"],
                    "path": "/v1/chat/completions"
                }
            },
            "enabled": true,
            "tags": [],
            "priority": 0
        }))
        .expect_status(201)
        .await;

    let json = resp.json();
    assert!(
        json["id"]
            .as_str()
            .unwrap()
            .starts_with("gts.x.core.oagw.route.v1~")
    );
}

// 7.12: GET upstreams with pagination.
#[tokio::test]
async fn list_upstreams_with_pagination() {
    let h = AppHarness::builder().build().await;

    // Create 3 upstreams.
    for i in 0..3 {
        h.facade()
            .create_upstream(
                h.security_context().clone(),
                oagw_sdk::CreateUpstreamRequest::builder(
                    oagw_sdk::Server {
                        endpoints: vec![oagw_sdk::Endpoint {
                            scheme: oagw_sdk::Scheme::Https,
                            host: format!("host{i}.example.com"),
                            port: 443,
                        }],
                    },
                    "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
                )
                .build(),
            )
            .await
            .unwrap();
    }

    let resp = h
        .api_v1()
        .list_upstreams()
        .with_query("limit", "2")
        .with_query("offset", "1")
        .expect_status(200)
        .await;

    let json = resp.json();
    let arr = json.as_array().unwrap();
    assert_eq!(arr.len(), 2);
}

// 7.13: Error mapper produces correct Problem Details.
#[tokio::test]
async fn error_mapper_produces_problem_details() {
    let h = AppHarness::builder().build().await;
    let fake_id = format_upstream_gts(Uuid::new_v4());

    let resp = h.api_v1().get_upstream(&fake_id).expect_status(404).await;

    resp.assert_header("content-type", "application/problem+json");
    let json = resp.json();
    assert!(json.get("type").is_some());
    assert!(json.get("title").is_some());
    assert!(json.get("status").is_some());
    assert!(json.get("detail").is_some());
}

/// Extract the UUID portion from a GTS ID string (e.g. "gts.x.core.oagw.upstream.v1~<uuid>")
/// and return it in standard hyphenated format.
fn gts_uuid(gts_id: &str) -> String {
    let raw = gts_id.rsplit('~').next().unwrap();
    Uuid::parse_str(raw).unwrap().to_string()
}

// 13.3: GET route by ID -> 200 with correct fields.
#[tokio::test]
async fn get_route_by_gts_id() {
    let h = AppHarness::builder().build().await;

    let upstream_resp = h
        .api_v1()
        .post_upstream()
        .with_body(serde_json::json!({
            "server": {
                "endpoints": [{"host": "api.openai.com", "port": 443, "scheme": "https"}]
            },
            "protocol": "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
            "enabled": true,
            "tags": []
        }))
        .expect_status(201)
        .await;

    let upstream_gts_id = upstream_resp.json()["id"].as_str().unwrap().to_string();
    let upstream_uuid = gts_uuid(&upstream_gts_id);

    let route_resp = h
        .api_v1()
        .post_route()
        .with_body(serde_json::json!({
            "upstream_id": upstream_uuid,
            "match": {
                "http": {
                    "methods": ["POST"],
                    "path": "/v1/chat/completions"
                }
            },
            "tags": ["test-tag"],
            "priority": 5,
            "enabled": true
        }))
        .expect_status(201)
        .await;

    let route_gts_id = route_resp.json()["id"].as_str().unwrap().to_string();

    let resp = h.api_v1().get_route(&route_gts_id).expect_status(200).await;

    let json = resp.json();
    assert_eq!(json["id"].as_str().unwrap(), route_gts_id);
    assert_eq!(json["upstream_id"].as_str().unwrap(), upstream_uuid);
    assert_eq!(json["priority"].as_i64().unwrap(), 5);
    assert!(json["enabled"].as_bool().unwrap());
    assert_eq!(json["tags"][0].as_str().unwrap(), "test-tag");
    assert_eq!(
        json["match"]["http"]["path"].as_str().unwrap(),
        "/v1/chat/completions"
    );
}

// 13.1: PUT route -> 200 with updated fields.
#[tokio::test]
async fn update_route_changes_fields() {
    let h = AppHarness::builder().build().await;

    let upstream_resp = h
        .api_v1()
        .post_upstream()
        .with_body(serde_json::json!({
            "server": {
                "endpoints": [{"host": "api.openai.com", "port": 443, "scheme": "https"}]
            },
            "protocol": "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
            "enabled": true,
            "tags": []
        }))
        .expect_status(201)
        .await;

    let upstream_json = upstream_resp.json();
    let upstream_uuid = gts_uuid(upstream_json["id"].as_str().unwrap());

    let route_resp = h
        .api_v1()
        .post_route()
        .with_body(serde_json::json!({
            "upstream_id": upstream_uuid,
            "match": {
                "http": {
                    "methods": ["POST"],
                    "path": "/v1/chat/completions"
                }
            },
            "tags": [],
            "priority": 0,
            "enabled": true
        }))
        .expect_status(201)
        .await;

    let route_gts_id = route_resp.json()["id"].as_str().unwrap().to_string();

    let resp = h
        .api_v1()
        .patch_route(&route_gts_id)
        .with_body(serde_json::json!({
            "priority": 10,
            "tags": ["updated"],
            "enabled": false
        }))
        .expect_status(200)
        .await;

    let json = resp.json();
    assert_eq!(json["id"].as_str().unwrap(), route_gts_id);
    assert_eq!(json["priority"].as_i64().unwrap(), 10);
    assert!(!json["enabled"].as_bool().unwrap());
    assert_eq!(json["tags"][0].as_str().unwrap(), "updated");
}

// 13.2: DELETE route -> 204, then GET returns 404.
#[tokio::test]
async fn delete_route_returns_204_then_get_returns_404() {
    let h = AppHarness::builder().build().await;

    let upstream_resp = h
        .api_v1()
        .post_upstream()
        .with_body(serde_json::json!({
            "server": {
                "endpoints": [{"host": "api.openai.com", "port": 443, "scheme": "https"}]
            },
            "protocol": "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
            "enabled": true,
            "tags": []
        }))
        .expect_status(201)
        .await;

    let upstream_json = upstream_resp.json();
    let upstream_uuid = gts_uuid(upstream_json["id"].as_str().unwrap());

    let route_resp = h
        .api_v1()
        .post_route()
        .with_body(serde_json::json!({
            "upstream_id": upstream_uuid,
            "match": {
                "http": {
                    "methods": ["GET"],
                    "path": "/v1/models"
                }
            },
            "tags": [],
            "priority": 0,
            "enabled": true
        }))
        .expect_status(201)
        .await;

    let route_gts_id = route_resp.json()["id"].as_str().unwrap().to_string();

    h.api_v1()
        .delete_route(&route_gts_id)
        .expect_status(204)
        .await;

    h.api_v1().get_route(&route_gts_id).expect_status(404).await;
}

// 13.4: List routes by upstream returns only that upstream's routes.
#[tokio::test]
async fn list_routes_filters_by_upstream() {
    let h = AppHarness::builder().build().await;

    // Create 2 upstreams with distinct hosts.
    let mut upstream_gts_ids = Vec::new();
    for host in ["api.openai.com", "api.anthropic.com"] {
        let resp = h
            .api_v1()
            .post_upstream()
            .with_body(serde_json::json!({
                "server": {
                    "endpoints": [{"host": host, "port": 443, "scheme": "https"}]
                },
                "protocol": "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
                "enabled": true,
                "tags": []
            }))
            .expect_status(201)
            .await;

        upstream_gts_ids.push(resp.json()["id"].as_str().unwrap().to_string());
    }

    let uuid_a = gts_uuid(&upstream_gts_ids[0]);
    let uuid_b = gts_uuid(&upstream_gts_ids[1]);

    // Create 2 routes on upstream A.
    for path in ["/v1/a1", "/v1/a2"] {
        h.api_v1()
            .post_route()
            .with_body(serde_json::json!({
                "upstream_id": uuid_a,
                "match": { "http": { "methods": ["GET"], "path": path } },
                "tags": [], "priority": 0, "enabled": true
            }))
            .expect_status(201)
            .await;
    }

    // Create 1 route on upstream B.
    h.api_v1()
        .post_route()
        .with_body(serde_json::json!({
            "upstream_id": uuid_b,
            "match": { "http": { "methods": ["GET"], "path": "/v1/b1" } },
            "tags": [], "priority": 0, "enabled": true
        }))
        .expect_status(201)
        .await;

    let resp = h
        .api_v1()
        .list_routes(&upstream_gts_ids[0])
        .expect_status(200)
        .await;

    let routes = resp.json();
    let arr = routes.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    for route in arr {
        assert_eq!(route["upstream_id"].as_str().unwrap(), uuid_a);
    }
}
