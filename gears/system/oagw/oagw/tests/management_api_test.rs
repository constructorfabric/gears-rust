//! Skipped under `--features fips`: `AppHarness` wires its token HTTP client
//! via `HttpClientConfig::for_testing()` (plaintext) against plaintext mocks
//! and does not install the FIPS crypto provider.

#![cfg(not(feature = "fips"))]

use oagw::test_support::{AppHarness, format_upstream_gts};
use oagw_sdk::HTTP_PROTOCOL_ID;
use toolkit_gts::gts_id;
use toolkit_gts::gts_uri;
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
            "protocol": HTTP_PROTOCOL_ID,
            "enabled": true,
            "tags": []
        }))
        .expect_status(201)
        .await;

    let json = resp.json();
    let id_str = json["id"].as_str().unwrap();
    assert_eq!(
        gts::GtsId::try_new(id_str)
            .expect("upstream id should be a valid GTS id")
            .get_type_id()
            .as_deref(),
        Some(gts_id!("cf.core.oagw.upstream.v1~"))
    );
    assert_eq!(json["alias"].as_str().unwrap(), "api.openai.com");
}

// 2.1: an endpoint without a port gets its scheme's default, 80 for `http`
// and 443 for `https`, and the response carries it (#4979).
#[tokio::test]
async fn create_upstream_omitted_port_defaults_per_scheme() {
    let h = AppHarness::builder().build().await;

    for (scheme, host, port) in [
        ("http", "plain.example.com", 80),
        ("https", "tls.example.com", 443),
    ] {
        let resp = h
            .api_v1()
            .post_upstream()
            .with_body(serde_json::json!({
                "server": {"endpoints": [{"host": host, "scheme": scheme}]},
                "protocol": HTTP_PROTOCOL_ID,
            }))
            .expect_status(201)
            .await;
        let json = resp.json();
        assert_eq!(
            json["server"]["endpoints"][0],
            serde_json::json!({"scheme": scheme, "host": host, "port": port})
        );
        assert_eq!(json["alias"], host);
    }
}

// 7.8: POST with missing server -> 422 (serde deserialization error).
#[tokio::test]
async fn create_upstream_missing_server_returns_422() {
    let h = AppHarness::builder().build().await;

    h.api_v1()
        .post_upstream()
        .with_body(serde_json::json!({
            "protocol": HTTP_PROTOCOL_ID
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
                HTTP_PROTOCOL_ID,
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
        gts_uri!("cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~")
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
                HTTP_PROTOCOL_ID,
            )
            .alias("openai")
            .build(),
        )
        .await
        .unwrap();

    let gts_id = format_upstream_gts(upstream.id);

    let resp = h
        .api_v1()
        .put_upstream(&gts_id)
        .with_body(serde_json::json!({
            "server": {
                "endpoints": [{"host": "10.0.0.1", "port": 443, "scheme": "https"}]
            },
            "protocol": HTTP_PROTOCOL_ID,
            "alias": "openai",
            "enabled": true,
            "tags": []
        }))
        .expect_status(200)
        .await;

    let json = resp.json();
    assert_eq!(json["id"].as_str().unwrap(), gts_id);
    assert_eq!(json["alias"].as_str().unwrap(), "openai");
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
                HTTP_PROTOCOL_ID,
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
                HTTP_PROTOCOL_ID,
            )
            .build(),
        )
        .await
        .unwrap();

    let resp = h
        .api_v1()
        .post_route()
        .with_body(serde_json::json!({
            "upstream_id": format_upstream_gts(upstream.id),
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
    assert_eq!(
        gts::GtsId::try_new(json["id"].as_str().unwrap())
            .expect("route id should be a valid GTS id")
            .get_type_id()
            .as_deref(),
        Some(gts_id!("cf.core.oagw.route.v1~"))
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
                    HTTP_PROTOCOL_ID,
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
            "protocol": HTTP_PROTOCOL_ID,
            "enabled": true,
            "tags": []
        }))
        .expect_status(201)
        .await;

    let upstream_gts_id = upstream_resp.json()["id"].as_str().unwrap().to_string();

    let route_resp = h
        .api_v1()
        .post_route()
        .with_body(serde_json::json!({
            "upstream_id": upstream_gts_id,
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
    assert_eq!(json["upstream_id"].as_str().unwrap(), upstream_gts_id);
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
            "protocol": HTTP_PROTOCOL_ID,
            "enabled": true,
            "tags": []
        }))
        .expect_status(201)
        .await;

    let upstream_gts_id = upstream_resp.json()["id"].as_str().unwrap().to_string();

    let route_resp = h
        .api_v1()
        .post_route()
        .with_body(serde_json::json!({
            "upstream_id": upstream_gts_id,
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
        .put_route(&route_gts_id)
        .with_body(serde_json::json!({
            "match": {
                "http": {
                    "methods": ["POST"],
                    "path": "/v1/chat/completions"
                }
            },
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
            "protocol": HTTP_PROTOCOL_ID,
            "enabled": true,
            "tags": []
        }))
        .expect_status(201)
        .await;

    let upstream_gts_id = upstream_resp.json()["id"].as_str().unwrap().to_string();

    let route_resp = h
        .api_v1()
        .post_route()
        .with_body(serde_json::json!({
            "upstream_id": upstream_gts_id,
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
                "protocol": HTTP_PROTOCOL_ID,
                "enabled": true,
                "tags": []
            }))
            .expect_status(201)
            .await;

        upstream_gts_ids.push(resp.json()["id"].as_str().unwrap().to_string());
    }

    // Create 2 routes on upstream A.
    for path in ["/v1/a1", "/v1/a2"] {
        h.api_v1()
            .post_route()
            .with_body(serde_json::json!({
                "upstream_id": &upstream_gts_ids[0],
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
            "upstream_id": &upstream_gts_ids[1],
            "match": { "http": { "methods": ["GET"], "path": "/v1/b1" } },
            "tags": [], "priority": 0, "enabled": true
        }))
        .expect_status(201)
        .await;

    let resp = h
        .api_v1()
        .list_routes(Some(&upstream_gts_ids[0]))
        .expect_status(200)
        .await;

    let routes = resp.json();
    let arr = routes.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    for route in arr {
        assert_eq!(route["upstream_id"].as_str().unwrap(), upstream_gts_ids[0]);
    }
}

// #4974: list parameters OAGW does not support are refused, instead of being
// dropped and answered with an unfiltered list.
/// The fields of the `UNSUPPORTED_QUERY_PARAM` violations of a problem.
fn unsupported_params(problem: &serde_json::Value) -> Vec<String> {
    problem["context"]["field_violations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|v| v["reason"] == "UNSUPPORTED_QUERY_PARAM")
        .map(|v| v["field"].as_str().unwrap_or_default().to_owned())
        .collect()
}

#[tokio::test]
async fn list_rejects_unsupported_query_params() {
    let h = AppHarness::builder().build().await;
    for (key, value) in [("$filter", "x"), ("$orderby", "alias"), ("tags", "a")] {
        let resp = h
            .api_v1()
            .list_upstreams()
            .with_query(key, value)
            .expect_status(400)
            .await;
        resp.assert_header("content-type", "application/problem+json");
        assert_eq!(unsupported_params(&resp.json()), [key], "{}", resp.text());
        let resp = h
            .api_v1()
            .list_routes(None)
            .with_query(key, value)
            .expect_status(400)
            .await;
        assert_eq!(unsupported_params(&resp.json()), [key], "{}", resp.text());
    }
    // Every unsupported key is reported once, in the order sent.
    let resp = h
        .api_v1()
        .list_upstreams()
        .with_query("$filter", "x")
        .with_query("limit", "10")
        .with_query("tags", "a")
        .with_query("$filter", "y")
        .expect_status(400)
        .await;
    assert_eq!(unsupported_params(&resp.json()), ["$filter", "tags"]);
    // A route-list key is not an upstream-list key.
    let resp = h
        .api_v1()
        .list_upstreams()
        .with_query("upstream_id", "x")
        .expect_status(400)
        .await;
    assert_eq!(unsupported_params(&resp.json()), ["upstream_id"]);
    // Unsupported keys are reported before a bad value of a supported one,
    // and a bad value alone is a problem response too.
    let resp = h
        .api_v1()
        .list_routes(None)
        .with_query("limit", "abc")
        .with_query("$filter", "x")
        .expect_status(400)
        .await;
    assert_eq!(unsupported_params(&resp.json()), ["$filter"]);
    let resp = h
        .api_v1()
        .list_upstreams()
        .with_query("limit", "abc")
        .expect_status(400)
        .await;
    resp.assert_header("content-type", "application/problem+json");
    assert_eq!(
        resp.json()["context"]["field_violations"][0]["field"],
        "query"
    );
    h.api_v1()
        .list_routes(None)
        .with_query("limit", "10")
        .with_query("offset", "0")
        .expect_status(200)
        .await;
}

// Scenarios 2.9 and 2.15 show these bodies: one of each problem shape a
// rejected write or list returns, compared whole.
#[tokio::test]
async fn rejected_writes_and_lists_return_the_documented_bodies() {
    const INVALID_ARGUMENT: &str =
        gts_uri!("cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~");
    const FAILED_PRECONDITION: &str =
        gts_uri!("cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~");
    let h = AppHarness::builder()
        .with_credentials(vec![("client-id".into(), "id".into())])
        .build()
        .await;
    let server = serde_json::json!({
        "endpoints": [{"host": "api.example.com", "port": 443, "scheme": "https"}]
    });
    let post = |body: serde_json::Value| {
        let mut body = body;
        body["protocol"] = HTTP_PROTOCOL_ID.into();
        body
    };

    // A rule without a field: a format error.
    let resp = h
        .api_v1()
        .post_upstream()
        .with_body(post(serde_json::json!({
            "server": {"endpoints": [{"host": "10.0.0.1", "port": 0, "scheme": "https"}]},
            "alias": "internal-api"
        })))
        .expect_status(400)
        .await;
    assert_eq!(
        resp.json(),
        serde_json::json!({
            "type": INVALID_ARGUMENT,
            "title": "Invalid Argument",
            "status": 400,
            "detail": "endpoint[0] port must be between 1 and 65535",
            "instance": "/oagw/v1/upstreams",
            "context": {
                "format": "endpoint[0] port must be between 1 and 65535",
                "resource_type": gts_id!("cf.core.oagw.proxy.v1~")
            }
        })
    );

    // A field violation.
    let resp = h
        .api_v1()
        .post_upstream()
        .with_body(post(serde_json::json!({
            "server": server,
            "auth": {"type": gts_id!("cf.core.oagw.auth_plugin.v1~cf.core.oagw.basic.v1")}
        })))
        .expect_status(400)
        .await;
    let implemented = [
        gts_id!("cf.core.oagw.auth_plugin.v1~cf.core.oagw.noop.v1"),
        gts_id!("cf.core.oagw.auth_plugin.v1~cf.core.oagw.apikey.v1"),
        gts_id!("cf.core.oagw.auth_plugin.v1~cf.core.oagw.oauth2_client_cred.v1"),
        gts_id!("cf.core.oagw.auth_plugin.v1~cf.core.oagw.oauth2_client_cred_basic.v1"),
        gts_id!("cf.core.oagw.auth_plugin.v1~cf.core.oagw.aws_sigv4.v1"),
    ];
    assert_eq!(
        resp.json(),
        serde_json::json!({
            "type": INVALID_ARGUMENT,
            "title": "Invalid Argument",
            "status": 400,
            "detail": "Request validation failed",
            "instance": "/oagw/v1/upstreams",
            "context": {
                "field_violations": [{
                    "field": "auth.type",
                    "reason": "NOT_IMPLEMENTED",
                    "description": format!(
                        "auth plugin '{}' is not implemented; use one of {implemented:?}",
                        gts_id!("cf.core.oagw.auth_plugin.v1~cf.core.oagw.basic.v1")
                    )
                }],
                "resource_type": gts_id!("cf.core.oagw.proxy.v1~")
            }
        })
    );

    // A malformed credential reference: a field violation on its key.
    let resp = h
        .api_v1()
        .post_upstream()
        .with_body(post(serde_json::json!({
            "server": server,
            "auth": {
                "type": gts_id!("cf.core.oagw.auth_plugin.v1~cf.core.oagw.apikey.v1"),
                "config": {"header": "authorization", "secret_ref": "cred://not a ref"}
            }
        })))
        .expect_status(400)
        .await;
    assert_eq!(
        resp.json(),
        serde_json::json!({
            "type": INVALID_ARGUMENT,
            "title": "Invalid Argument",
            "status": 400,
            "detail": "Request validation failed",
            "instance": "/oagw/v1/upstreams",
            "context": {
                "field_violations": [{
                    "field": "auth.config.secret_ref",
                    "reason": "VALIDATION",
                    "description": "auth.config.secret_ref: invalid credential reference: \
                                    contains invalid characters; only [a-zA-Z0-9_-] are allowed"
                }],
                "resource_type": gts_id!("cf.core.oagw.proxy.v1~")
            }
        })
    );

    // A credential reference the store does not have: a precondition.
    let resp = h
        .api_v1()
        .post_upstream()
        .with_body(post(serde_json::json!({
            "server": server,
            "auth": {
                "type": gts_id!("cf.core.oagw.auth_plugin.v1~cf.core.oagw.oauth2_client_cred.v1"),
                "config": {
                    "token_endpoint": "https://idp.example.com/token",
                    "client_id_ref": "cred://client-id",
                    "client_secret_ref": "cred://missing"
                }
            }
        })))
        .expect_status(400)
        .await;
    assert_eq!(
        resp.json(),
        serde_json::json!({
            "type": FAILED_PRECONDITION,
            "title": "Failed Precondition",
            "status": 400,
            "detail": "Operation precondition not met",
            "instance": "/oagw/v1/upstreams",
            "context": {
                "violations": [{
                    "type": "STATE",
                    "subject": "auth.config.client_secret_ref",
                    "description": "credential reference is not accessible to this tenant \
                                    (not provisioned yet, or not shared)"
                }],
                "resource_type": gts_id!("cf.core.oagw.upstream.v1~")
            }
        })
    );

    // An unsupported list parameter names the listed resource.
    for (resp, instance, resource_type, supported) in [
        (
            h.api_v1()
                .list_upstreams()
                .with_query("$filter", "x")
                .expect_status(400)
                .await,
            "/oagw/v1/upstreams",
            gts_id!("cf.core.oagw.upstream.v1~"),
            "limit, offset",
        ),
        (
            h.api_v1()
                .list_routes(None)
                .with_query("$filter", "x")
                .expect_status(400)
                .await,
            "/oagw/v1/routes",
            gts_id!("cf.core.oagw.route.v1~"),
            "upstream_id, limit, offset",
        ),
    ] {
        assert_eq!(
            resp.json(),
            serde_json::json!({
                "type": INVALID_ARGUMENT,
                "title": "Invalid Argument",
                "status": 400,
                "detail": "Request validation failed",
                "instance": instance,
                "context": {
                    "field_violations": [{
                        "field": "$filter",
                        "reason": "UNSUPPORTED_QUERY_PARAM",
                        "description": format!(
                            "query parameter '$filter' is not supported; supported: {supported}"
                        )
                    }],
                    "resource_type": resource_type
                }
            })
        );
    }
}
