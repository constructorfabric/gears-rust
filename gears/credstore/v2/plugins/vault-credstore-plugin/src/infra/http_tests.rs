// Created: 2026-10-03 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
use httpmock::prelude::*;

use super::*;
use crate::config::VaultToken;

fn transport_for(server: &MockServer) -> ReqwestTransport {
    let cfg = VaultCredStorePluginConfig {
        address: format!("http://127.0.0.1:{}", server.port()),
        token: Some(VaultToken::from("test-token")),
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
        token: Some(VaultToken::from("test-token")),
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
        token: Some(VaultToken::from("test-token")),
        // Windows retries a refused TCP connect for ~2 s, so a shorter request timeout fires
        // first and reports `Timeout` instead of `Connect`.
        timeout_secs: 10,
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

// -- 403: token re-read -----------------------------------------------------------

fn file_transport(server: &MockServer, path: &std::path::Path) -> ReqwestTransport {
    let cfg = VaultCredStorePluginConfig {
        address: format!("http://127.0.0.1:{}", server.port()),
        token_file: Some(path.to_string_lossy().into_owned()),
        ..VaultCredStorePluginConfig::default()
    };
    ReqwestTransport::from_config(&cfg).expect("builds")
}

/// A `403` for `old`, a `200` for `new`: Vault after the old token was revoked.
fn mock_rotation<'a>(
    server: &'a MockServer,
    old: &str,
    new: &str,
) -> (httpmock::Mock<'a>, httpmock::Mock<'a>) {
    let rejected = server.mock(|when, then| {
        when.method(GET).header("X-Vault-Token", old);
        then.status(403)
            .json_body(serde_json::json!({"errors": ["permission denied"]}));
    });
    let accepted = server.mock(|when, then| {
        when.method(GET).header("X-Vault-Token", new);
        then.status(200).body("ok");
    });
    (rejected, accepted)
}

#[tokio::test]
async fn forbidden_re_reads_the_token_file_and_retries_once_with_the_new_token() {
    let server = MockServer::start();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("token");
    std::fs::write(&path, "s.old\n").expect("write");
    let (rejected, accepted) = mock_rotation(&server, "s.old", "s.new");
    let transport = file_transport(&server, &path);

    // First request: the old token works (nothing rotated yet).
    let first = server.mock(|when, then| {
        when.method(POST).header("X-Vault-Token", "s.old");
        then.status(200).body("first");
    });
    let got = transport
        .send(request(HttpMethod::Post, &server, Some("{}")))
        .await
        .expect("ok");
    assert_eq!((got.status, got.body.as_str()), (200, "first"));
    first.assert();

    // A sidecar rewrites the file and the old token is revoked.
    std::fs::write(&path, "s.new\n").expect("write");
    let got = transport
        .send(request(HttpMethod::Get, &server, None))
        .await
        .expect("ok");
    assert_eq!((got.status, got.body.as_str()), (200, "ok"));
    rejected.assert_calls(1);
    accepted.assert_calls(1);

    // The new token is cached: the next request does not hit the 403 again.
    transport
        .send(request(HttpMethod::Get, &server, None))
        .await
        .expect("ok");
    rejected.assert_calls(1);
    accepted.assert_calls(2);
}

#[tokio::test]
async fn forbidden_retry_resends_the_body() {
    let server = MockServer::start();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("token");
    std::fs::write(&path, "s.old").expect("write");
    let transport = file_transport(&server, &path);
    // Prime the cache with the old token, then rotate.
    let mut prime = server.mock(|when, then| {
        when.method(GET).header("X-Vault-Token", "s.old");
        then.status(200);
    });
    transport
        .send(request(HttpMethod::Get, &server, None))
        .await
        .expect("ok");
    prime.assert();
    prime.delete();
    std::fs::write(&path, "s.new").expect("write");

    let rejected = server.mock(|when, then| {
        when.method(POST).header("X-Vault-Token", "s.old");
        then.status(403);
    });
    let accepted = server.mock(|when, then| {
        when.method(POST)
            .header("X-Vault-Token", "s.new")
            .json_body(serde_json::json!({"data": {"value": "aGk="}}));
        then.status(200).body("written");
    });
    let got = transport
        .send(request(
            HttpMethod::Post,
            &server,
            Some(r#"{"data":{"value":"aGk="}}"#),
        ))
        .await
        .expect("ok");
    assert_eq!((got.status, got.body.as_str()), (200, "written"));
    rejected.assert();
    accepted.assert();
}

#[tokio::test]
async fn forbidden_with_an_unchanged_token_file_is_returned_after_one_request() {
    let server = MockServer::start();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("token");
    std::fs::write(&path, "s.same").expect("write");
    let forbidden = server.mock(|when, then| {
        when.method(GET);
        then.status(403);
    });
    let got = file_transport(&server, &path)
        .send(request(HttpMethod::Get, &server, None))
        .await
        .expect("a 403 is a response");
    assert_eq!(got.status, 403);
    forbidden.assert_calls(1);
}

#[tokio::test]
async fn forbidden_twice_is_returned_and_never_retried_a_second_time() {
    let server = MockServer::start();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("token");
    std::fs::write(&path, "s.old").expect("write");
    let transport = file_transport(&server, &path);
    // Both tokens are rejected.
    let forbidden = server.mock(|when, then| {
        when.method(GET);
        then.status(403);
    });
    transport
        .send(request(HttpMethod::Get, &server, None))
        .await
        .expect("first read of the file, 403, same token");
    forbidden.assert_calls(1);

    std::fs::write(&path, "s.new").expect("write");
    let got = transport
        .send(request(HttpMethod::Get, &server, None))
        .await
        .expect("a 403 is a response");
    assert_eq!(got.status, 403, "the retry's 403 is final");
    forbidden.assert_calls(3);
}

#[tokio::test]
async fn forbidden_with_an_inline_token_is_not_retried() {
    let server = MockServer::start();
    let forbidden = server.mock(|when, then| {
        when.method(GET);
        then.status(403);
    });
    let got = transport_for(&server)
        .send(request(HttpMethod::Get, &server, None))
        .await
        .expect("a 403 is a response");
    assert_eq!(got.status, 403);
    forbidden.assert_calls(1);
}

#[tokio::test]
async fn forbidden_with_an_unreadable_token_file_is_returned() {
    let server = MockServer::start();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("token");
    std::fs::write(&path, "s.old").expect("write");
    let transport = file_transport(&server, &path);
    let prime = server.mock(|when, then| {
        when.method(GET);
        then.status(403);
    });
    // Remove the file after the first read: the re-read fails.
    transport
        .send(request(HttpMethod::Get, &server, None))
        .await
        .expect("ok");
    std::fs::remove_file(&path).expect("remove");
    let got = transport
        .send(request(HttpMethod::Get, &server, None))
        .await
        .expect("a 403 is a response");
    assert_eq!(got.status, 403);
    prime.assert_calls(2);
}

#[tokio::test]
async fn a_missing_token_file_is_a_credentials_error_and_sends_nothing() {
    let server = MockServer::start();
    let any = server.mock(|when, then| {
        when.any_request();
        then.status(200);
    });
    let dir = tempfile::tempdir().expect("tempdir");
    let transport = file_transport(&server, &dir.path().join("missing"));
    let err = transport
        .send(request(HttpMethod::Get, &server, None))
        .await
        .unwrap_err();
    assert_eq!(err, TransportError::Credentials);
    any.assert_calls(0);
}

#[tokio::test]
async fn a_token_file_that_appears_later_is_used() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(GET).header("X-Vault-Token", "s.late");
        then.status(200);
    });
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("token");
    let transport = file_transport(&server, &path);
    assert_eq!(
        transport
            .send(request(HttpMethod::Get, &server, None))
            .await
            .unwrap_err(),
        TransportError::Credentials
    );
    std::fs::write(&path, "s.late\n").expect("write");
    let got = transport
        .send(request(HttpMethod::Get, &server, None))
        .await
        .expect("ok");
    assert_eq!(got.status, 200);
    mock.assert();
}

#[test]
fn from_config_requires_exactly_one_token_source() {
    assert!(ReqwestTransport::from_config(&VaultCredStorePluginConfig::default()).is_err());
}

#[test]
fn token_source_is_reported_for_logging() {
    let cfg = VaultCredStorePluginConfig {
        token: Some(VaultToken::from("t")),
        ..VaultCredStorePluginConfig::default()
    };
    assert_eq!(
        ReqwestTransport::from_config(&cfg)
            .expect("builds")
            .token_source(),
        "token"
    );
}

// -- Retry-After -------------------------------------------------------------------

#[tokio::test]
async fn retry_after_in_seconds_is_parsed() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET);
        then.status(503).header("Retry-After", " 7 ");
    });
    let got = transport_for(&server)
        .send(request(HttpMethod::Get, &server, None))
        .await
        .expect("ok");
    assert_eq!(got.retry_after, Some(Duration::from_secs(7)));
}

#[tokio::test]
async fn retry_after_is_capped_and_other_forms_are_ignored() {
    let server = MockServer::start();
    let transport = transport_for(&server);
    for (header, expected) in [
        ("86400", Some(Duration::from_mins(5))),
        ("Wed, 21 Oct 2026 07:28:00 GMT", None),
        ("soon", None),
    ] {
        let mut mock = server.mock(|when, then| {
            when.method(GET);
            then.status(429).header("Retry-After", header);
        });
        let got = transport
            .send(request(HttpMethod::Get, &server, None))
            .await
            .expect("ok");
        assert_eq!(got.retry_after, expected, "{header}");
        mock.delete();
    }
}

#[tokio::test]
async fn no_retry_after_header_is_none() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET);
        then.status(200);
    });
    let got = transport_for(&server)
        .send(request(HttpMethod::Get, &server, None))
        .await
        .expect("ok");
    assert_eq!(got.retry_after, None);
}
