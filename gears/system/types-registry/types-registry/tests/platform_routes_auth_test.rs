#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "integration tests: a failed fixture or assertion step is a test failure"
)]
//! REST auth matrix through the `OoP` TCP listener (D17/D20, T33/T35).
//! Each route admits one plane and returns canonical 401 reasons on refusal.
//! Tenant bearers cannot mutate or read operations; a missing authenticator
//! disables only its plane.
//!
//! See `api-gateway/tests/auth_middleware.rs` for gateway plane enforcement,
//! including missing internal auth, and e2e `TR-PLANE` for a configured gateway.

use serde_json::{Value, json};
use toolkit_gts::gts_id;
use toolkit_http::{HttpClient, HttpClientBuilder, RequestBuilder};
use toolkit_security::constants::INTERNAL_TOKEN_HEADER;

mod common;
use common::platform_server::{
    INTERNAL_TOKEN, PlatformServer, TENANT_BEARER, serve, serve_without_internal_authenticator,
    serve_without_tenant_authenticator,
};

const CF_TYPE: &str = gts_id!("cf.core.example.type.v1~");
const PLATFORM: &str = "/types-registry/v2/platform";
const TENANT: &str = "/types-registry/v2";

/// The credentials one request presents.
#[derive(Clone, Copy, Debug)]
enum Credentials {
    None,
    Token(&'static str),
    Bearer(&'static str),
    Both(&'static str, &'static str),
}

fn http() -> HttpClient {
    HttpClientBuilder::new()
        .retry(None)
        .build()
        .expect("an HTTP client")
}

fn with(builder: RequestBuilder, credentials: Credentials) -> RequestBuilder {
    match credentials {
        Credentials::None => builder,
        Credentials::Token(token) => builder.header(INTERNAL_TOKEN_HEADER, token),
        Credentials::Bearer(bearer) => builder.bearer_auth(bearer),
        Credentials::Both(token, bearer) => builder
            .header(INTERNAL_TOKEN_HEADER, token)
            .bearer_auth(bearer),
    }
}

fn schema(gts_id: &str) -> Value {
    json!({
        "$id": format!("gts://{gts_id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
    })
}

/// One request per route, platform and tenant; the mutations are distinct per call so a
/// served one never collides with an earlier one.
async fn send(
    server: &PlatformServer,
    route: &str,
    credentials: Credentials,
    nonce: usize,
) -> (u16, Value) {
    let http = http();
    let base = &server.base_url;
    let key = format!("auth-matrix-{nonce}");
    let builder = match route {
        "submit" => http
            .post(&format!("{base}{PLATFORM}/entities"))
            .header("idempotency-key", &key)
            .json(&json!({ "items": [{ "gts_id": CF_TYPE, "content": schema(CF_TYPE) }], "dry_run": true }))
            .unwrap(),
        "batch_delete" => http
            .post(&format!("{base}{PLATFORM}/entities:batchDelete"))
            .header("idempotency-key", &key)
            .json(&json!({ "items": [{ "entity_key": CF_TYPE, "expected_resource_version": 1 }], "dry_run": true }))
            .unwrap(),
        "delete" => http
            .delete(&format!(
                "{base}{PLATFORM}/entities/{CF_TYPE}?expected_resource_version=1&dry_run=true"
            ))
            .header("idempotency-key", &key),
        "operation" => http.get(&format!(
            "{base}{PLATFORM}/operations/00000000-0000-0000-0000-000000000001"
        )),
        "platform_get" => http.get(&format!("{base}{PLATFORM}/entities/{CF_TYPE}")),
        "platform_batch_get" => http
            .post(&format!("{base}{PLATFORM}/entities:batchGet"))
            .json(&json!({ "items": [{ "entity_key": CF_TYPE }] }))
            .unwrap(),
        "platform_list" => http.get(&format!("{base}{PLATFORM}/entities")),
        "tenant_get" => http.get(&format!("{base}{TENANT}/entities/{CF_TYPE}")),
        "tenant_batch_get" => http
            .post(&format!("{base}{TENANT}/entities:batchGet"))
            .json(&json!({ "items": [{ "entity_key": CF_TYPE }] }))
            .unwrap(),
        "tenant_list" => http.get(&format!("{base}{TENANT}/entities")),
        "legacy_list" => http.get(&format!("{base}/types-registry/v1/entities")),
        other => panic!("unknown route {other}"),
    };
    let response = with(builder, credentials).send().await.expect("a response");
    let status = response.status().as_u16();
    let bytes = response.bytes().await.expect("a body");
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

const PLATFORM_ROUTES: [&str; 7] = [
    "submit",
    "batch_delete",
    "delete",
    "operation",
    "platform_get",
    "platform_batch_get",
    "platform_list",
];
const TENANT_ROUTES: [&str; 3] = ["tenant_get", "tenant_batch_get", "tenant_list"];

/// A canonical `401` with its machine-readable reason, never a `500`.
fn assert_refused(
    route: &str,
    credentials: Credentials,
    (status, body): &(u16, Value),
    reason: &str,
) {
    assert_eq!(*status, 401, "{route} with {credentials:?}: {body}");
    assert_eq!(
        body["context"]["reason"],
        json!(reason),
        "{route} with {credentials:?}: {body}"
    );
}

/// A served answer: anything the handler decides, but no authentication refusal.
fn assert_served(route: &str, credentials: Credentials, (status, body): &(u16, Value)) {
    assert!(
        matches!(status, 200 | 202 | 404),
        "{route} with {credentials:?} must reach its handler: {status} {body}"
    );
}

#[tokio::test]
async fn a_platform_route_serves_a_valid_internal_token_only() {
    let server = serve().await;
    let mut nonce = 0;
    for route in PLATFORM_ROUTES {
        for (credentials, reason) in [
            (Credentials::None, Some("MISSING_INTERNAL_TOKEN")),
            // A bearer alone is dropped unvalidated, so the token is missing.
            (
                Credentials::Bearer(TENANT_BEARER),
                Some("MISSING_INTERNAL_TOKEN"),
            ),
            (
                Credentials::Bearer("forged-bearer"),
                Some("MISSING_INTERNAL_TOKEN"),
            ),
            (
                Credentials::Token("forged-token"),
                Some("INTERNAL_AUTH_FAILED"),
            ),
            (Credentials::Token(INTERNAL_TOKEN), None),
        ] {
            nonce += 1;
            let answer = send(&server, route, credentials, nonce).await;
            match reason {
                Some(reason) => assert_refused(route, credentials, &answer, reason),
                None => assert_served(route, credentials, &answer),
            }
        }
    }
}

#[tokio::test]
async fn a_tenant_read_serves_a_valid_bearer_only() {
    let server = serve().await;
    let mut nonce = 0;
    for route in TENANT_ROUTES {
        for (credentials, reason) in [
            (Credentials::None, Some("MISSING_BEARER")),
            // A token alone is dropped unvalidated on a tenant route, so the bearer is missing.
            (Credentials::Token(INTERNAL_TOKEN), Some("MISSING_BEARER")),
            (Credentials::Bearer("forged-bearer"), Some("AUTHN_FAILED")),
            (Credentials::Bearer(TENANT_BEARER), None),
        ] {
            nonce += 1;
            let answer = send(&server, route, credentials, nonce).await;
            match reason {
                Some(reason) => assert_refused(route, credentials, &answer, reason),
                None => assert_served(route, credentials, &answer),
            }
        }
    }
}

/// On every host topology — both authenticators, or either missing — a request carrying
/// both credentials is refused before either is looked at.
#[tokio::test]
async fn both_credentials_are_refused_on_every_route_whatever_their_validity() {
    let mut nonce = 0;
    for (topology, server) in [
        ("both authenticators", serve().await),
        (
            "no internal authenticator",
            serve_without_internal_authenticator().await,
        ),
        (
            "no tenant authenticator",
            serve_without_tenant_authenticator().await,
        ),
    ] {
        for route in PLATFORM_ROUTES.into_iter().chain(TENANT_ROUTES) {
            for (token, bearer) in [
                (INTERNAL_TOKEN, TENANT_BEARER),
                (INTERNAL_TOKEN, "forged-bearer"),
                ("forged-token", TENANT_BEARER),
                ("forged-token", "forged-bearer"),
            ] {
                nonce += 1;
                let credentials = Credentials::Both(token, bearer);
                let answer = send(&server, route, credentials, nonce).await;
                assert_eq!(answer.0, 401, "{topology}, {route} with {credentials:?}");
                assert_refused(route, credentials, &answer, "MIXED_CREDENTIALS");
            }
        }
    }
}

/// D17: a tenant bearer cannot mutate by any path, nor read an operation.
#[tokio::test]
async fn a_tenant_bearer_mutates_nothing() {
    let server = serve().await;
    let http = http();
    let base = &server.base_url;
    for (method, path) in [
        ("POST", format!("{PLATFORM}/entities")),
        ("POST", format!("{TENANT}/entities")),
        ("POST", format!("{TENANT}/entities:batchDelete")),
        (
            "DELETE",
            format!("{TENANT}/entities/{CF_TYPE}?expected_resource_version=1"),
        ),
        (
            "GET",
            format!("{TENANT}/operations/00000000-0000-0000-0000-000000000001"),
        ),
    ] {
        let url = format!("{base}{path}");
        let builder = match method {
            "POST" => http
                .post(&url)
                .json(&json!({ "items": [{ "gts_id": CF_TYPE, "content": schema(CF_TYPE) }] }))
                .unwrap(),
            "DELETE" => http.delete(&url),
            _ => http.get(&url),
        };
        let response = builder
            .header("idempotency-key", "tenant-write")
            .bearer_auth(TENANT_BEARER)
            .send()
            .await
            .expect("a response");
        let status = response.status().as_u16();
        assert!(
            matches!(status, 401 | 404 | 405),
            "{method} {path} must not be served to a tenant bearer, got {status}"
        );
    }
    let (status, body) = send(
        &server,
        "platform_get",
        Credentials::Token(INTERNAL_TOKEN),
        0,
    )
    .await;
    assert_eq!(status, 404, "nothing was registered: {body}");
}

/// A standalone registry without a tenant authenticator refuses every tenant route with a
/// canonical `401` before its handler runs — never a `500` from the handler's extractor,
/// never a served request — and still serves the platform routes. The listener refuses
/// them before the registry's own gate is reached.
#[tokio::test]
async fn without_a_tenant_authenticator_tenant_routes_are_refused_and_platform_routes_served() {
    let server = serve_without_tenant_authenticator().await;
    let mut nonce = 0;
    for route in TENANT_ROUTES.into_iter().chain(["legacy_list"]) {
        for credentials in [
            Credentials::None,
            Credentials::Bearer(TENANT_BEARER),
            Credentials::Bearer("forged-bearer"),
            Credentials::Token(INTERNAL_TOKEN),
        ] {
            nonce += 1;
            let answer = send(&server, route, credentials, nonce).await;
            assert_refused(route, credentials, &answer, "TENANT_AUTH_NOT_CONFIGURED");
        }
    }
    for route in PLATFORM_ROUTES {
        nonce += 1;
        let answer = send(&server, route, Credentials::Token(INTERNAL_TOKEN), nonce).await;
        assert_served(route, Credentials::Token(INTERNAL_TOKEN), &answer);
    }
    nonce += 1;
    let mixed = Credentials::Both(INTERNAL_TOKEN, TENANT_BEARER);
    let answer = send(&server, "tenant_get", mixed, nonce).await;
    assert_refused("tenant_get", mixed, &answer, "MIXED_CREDENTIALS");
}

/// A host with no internal authenticator refuses every platform route with a canonical
/// `401` whatever it is sent — a valid token included, since nothing can validate it — and
/// still serves the tenant reads to a valid bearer.
#[tokio::test]
async fn without_an_internal_authenticator_platform_routes_are_refused_and_tenant_reads_served() {
    let server = serve_without_internal_authenticator().await;
    let mut nonce = 0;
    for route in PLATFORM_ROUTES {
        for (credentials, reason) in [
            (Credentials::None, "MISSING_INTERNAL_TOKEN"),
            // Presented, but no installed plane can validate it.
            (
                Credentials::Token(INTERNAL_TOKEN),
                "UNVERIFIED_INTERNAL_TOKEN",
            ),
            (
                Credentials::Token("forged-token"),
                "UNVERIFIED_INTERNAL_TOKEN",
            ),
            // Dropped unvalidated on a platform route, so the token is missing.
            (Credentials::Bearer(TENANT_BEARER), "MISSING_INTERNAL_TOKEN"),
        ] {
            nonce += 1;
            let answer = send(&server, route, credentials, nonce).await;
            assert_refused(route, credentials, &answer, reason);
        }
    }
    for route in TENANT_ROUTES {
        nonce += 1;
        let answer = send(&server, route, Credentials::Bearer(TENANT_BEARER), nonce).await;
        assert_served(route, Credentials::Bearer(TENANT_BEARER), &answer);
    }
}
