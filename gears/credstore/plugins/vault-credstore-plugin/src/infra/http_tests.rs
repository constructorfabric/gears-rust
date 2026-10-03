use httpmock::prelude::*;

use super::*;
use crate::config::VaultToken;

fn transport_for(server: &MockServer) -> ReqwestTransport {
    let cfg = VaultCredStorePluginConfig {
        address: format!("http://127.0.0.1:{}", server.port()),
        token: VaultToken::from("test-token"),
        ..VaultCredStorePluginConfig::default()
    };
    ReqwestTransport::from_config(&cfg).expect("builds")
}

fn request(method: HttpMethod, server: &MockServer, json_body: Option<&str>) -> VaultRequest {
    VaultRequest {
        method,
        url: server.url("/v1/secret/data/credstore/t/r"),
        json_body: json_body.map(str::to_owned),
    }
}

#[tokio::test]
async fn sends_token_header_and_returns_status_and_body() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(GET)
            .path("/v1/secret/data/credstore/t/r")
            .header("X-Vault-Token", "test-token");
        then.status(404).body("gone");
    });

    let got = transport_for(&server)
        .send(request(HttpMethod::Get, &server, None))
        .await
        .expect("a 404 is a response, not an error");
    assert_eq!(got.status, 404);
    assert_eq!(got.body, "gone");
    mock.assert();
}

#[tokio::test]
async fn sends_namespace_header_only_when_configured() {
    let server = MockServer::start();
    let with_ns = server.mock(|when, then| {
        when.method(GET).header("X-Vault-Namespace", "team-a");
        then.status(200);
    });

    let cfg = VaultCredStorePluginConfig {
        token: VaultToken::from("test-token"),
        namespace: Some("team-a".to_owned()),
        ..VaultCredStorePluginConfig::default()
    };
    let transport = ReqwestTransport::from_config(&cfg).expect("builds");
    let got = transport
        .send(request(HttpMethod::Get, &server, None))
        .await
        .expect("ok");
    assert_eq!(got.status, 200);
    with_ns.assert();
}

#[tokio::test]
async fn posts_the_json_body_with_content_type() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/v1/secret/data/credstore/t/r")
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({"data": {"value": "aGk="}}));
        then.status(200).body("{}");
    });

    let got = transport_for(&server)
        .send(request(
            HttpMethod::Post,
            &server,
            Some(r#"{"data":{"value":"aGk="}}"#),
        ))
        .await
        .expect("ok");
    assert_eq!(got.status, 200);
    mock.assert();
}

#[tokio::test]
async fn delete_verb_is_used_for_delete_requests() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(DELETE).path("/v1/secret/data/credstore/t/r");
        then.status(204);
    });

    let got = transport_for(&server)
        .send(request(HttpMethod::Delete, &server, None))
        .await
        .expect("ok");
    assert_eq!(got.status, 204);
    mock.assert();
}

#[tokio::test]
async fn connection_refused_is_a_connect_error() {
    // Nothing listening on this port - connect must fail.
    let cfg = VaultCredStorePluginConfig {
        token: VaultToken::from("test-token"),
        timeout_secs: 2,
        ..VaultCredStorePluginConfig::default()
    };
    let transport = ReqwestTransport::from_config(&cfg).expect("builds");
    let err = transport
        .send(VaultRequest {
            method: HttpMethod::Get,
            url: "http://127.0.0.1:1/v1/secret/data/credstore/t/r".to_owned(),
            json_body: None,
        })
        .await
        .unwrap_err();
    assert_eq!(err, TransportError::Connect);
}
