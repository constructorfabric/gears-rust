use std::sync::Arc;
use std::time::Duration;

use authn_resolver_sdk::AuthNResolverClient;
use http::StatusCode;
use httpmock::prelude::*;
use modkit_http::{HttpError, InvalidUriKind};
use serde_json::json;
use usage_collector_sdk::models::{Subject, UsageKind, UsageRecord};
use usage_collector_sdk::{
    ModuleConfig, ModuleConfigError, UsageCollectorClientV1, UsageCollectorError, UsageRecordError,
};
use uuid::Uuid;

use super::super::test_support::{MockAuthN, test_cfg, test_record};
use super::{UsageCollectorRestClient, map_transport_err};

// --- Integration tests with mock HTTP server ---

fn make_client(base_url: &str, authn: Arc<dyn AuthNResolverClient>) -> UsageCollectorRestClient {
    UsageCollectorRestClient::new(&test_cfg(base_url), authn).unwrap()
}

#[tokio::test]
async fn new_returns_invalid_uri_for_opaque_collector_url() {
    // `cannot_be_a_base()` URLs (e.g. `data:`) parse as a valid `Url` but cannot
    // host path segments. `UsageCollectorRestClient::new` must surface that as
    // `HttpError::InvalidUri` rather than panicking. This path is not reachable
    // through the normal config flow because `validate()` rejects it earlier,
    // but `new()` does not require `validate()` to have run.
    let cfg = test_cfg("data:text/plain,x");
    let Err(err) = UsageCollectorRestClient::new(&cfg, MockAuthN::with_token("tok")) else {
        panic!("expected HttpError::InvalidUri, got Ok(...)");
    };
    let HttpError::InvalidUri { url, kind, .. } = err else {
        panic!("expected HttpError::InvalidUri, got: {err:?}");
    };
    assert_eq!(url, "data:text/plain,x");
    assert!(matches!(kind, InvalidUriKind::ParseError));
}

#[tokio::test]
async fn create_usage_record_success_on_204() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST).path("/usage-collector/v1/records");
        then.status(204);
    });

    let client = make_client(&server.base_url(), MockAuthN::with_token("tok"));
    assert!(client.create_usage_record(test_record()).await.is_ok());
    mock.assert();
}

#[tokio::test]
async fn create_usage_record_sends_bearer_token_header() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/usage-collector/v1/records")
            .header("authorization", "Bearer my-token");
        then.status(204);
    });

    let client = make_client(&server.base_url(), MockAuthN::with_token("my-token"));
    client.create_usage_record(test_record()).await.unwrap();
    mock.assert();
}

// Two representative AuthN-failure cases — a permanent rejection
// (`Unauthorized`) and a transient one (`ServiceUnavailable`). Both must
// collapse to `ServiceUnavailable` at the REST boundary so the outbox retries,
// and the Display must preserve the underlying AuthN cause for diagnostics.
// The remaining AuthN error variants (NoPlugin / TokenAcquisitionFailed /
// without_token) are covered by `bearer_token_auth_layer_tests` where each
// variant's Transport wrapping is asserted distinctly.

#[tokio::test]
async fn create_usage_record_authn_unauthorized_returns_service_unavailable() {
    let server = MockServer::start();
    let client = make_client(&server.base_url(), MockAuthN::unauthorized());

    let err = client.create_usage_record(test_record()).await.unwrap_err();
    assert!(matches!(
        err,
        UsageCollectorError::ServiceUnavailable { .. }
    ));
    let msg = err.to_string();
    assert!(
        msg.contains("unauthorized") && msg.contains("bad credentials"),
        "Display must preserve the AuthN cause, got: {msg}"
    );
}

#[tokio::test]
async fn create_usage_record_authn_service_unavailable_returns_service_unavailable() {
    // ServiceUnavailable is transient: the identity service is temporarily unreachable.
    let server = MockServer::start();
    let client = make_client(&server.base_url(), MockAuthN::service_unavailable());

    let err = client.create_usage_record(test_record()).await.unwrap_err();
    assert!(matches!(
        err,
        UsageCollectorError::ServiceUnavailable { .. }
    ));
    let msg = err.to_string();
    assert!(
        msg.contains("service unavailable")
            && msg.contains("identity service temporarily unreachable"),
        "Display must preserve the AuthN cause, got: {msg}"
    );
}

#[tokio::test]
async fn create_usage_record_server_401_returns_service_unavailable() {
    // 401 is transient — expired token; next attempt acquires a fresh bearer token (inst-rem-9)
    let server = MockServer::start();
    let _mock = server.mock(|when, then| {
        when.method(POST).path("/usage-collector/v1/records");
        then.status(401).body("token-expired-marker");
    });

    let client = make_client(&server.base_url(), MockAuthN::with_token("tok"));
    let err = client.create_usage_record(test_record()).await.unwrap_err();
    assert!(matches!(
        err,
        UsageCollectorError::ServiceUnavailable { .. }
    ));
    let msg = err.to_string();
    assert!(
        msg.contains("401") && msg.contains("token-expired-marker"),
        "Display must include the HTTP status and a marker from the response body, got: {msg}"
    );
}

#[tokio::test]
async fn create_usage_record_server_403_returns_permission_denied() {
    let server = MockServer::start();
    let _mock = server.mock(|when, then| {
        when.method(POST).path("/usage-collector/v1/records");
        then.status(403).body("forbidden-marker");
    });

    let client = make_client(&server.base_url(), MockAuthN::with_token("tok"));
    let err = client.create_usage_record(test_record()).await.unwrap_err();
    // The PDP-denial reason must capture the upstream body so operators can see
    // which policy fired; a regression that drops the reason text would still
    // satisfy a bare `matches!` check on the variant.
    let UsageCollectorError::PermissionDenied { ctx, .. } = &err else {
        panic!("expected PermissionDenied, got: {err:?}");
    };
    assert!(
        ctx.reason.contains("403") && ctx.reason.contains("forbidden-marker"),
        "PermissionDenied reason must include the HTTP status and a marker from the response body, got: {}",
        ctx.reason
    );
}

#[tokio::test]
async fn create_usage_record_server_429_returns_resource_exhausted() {
    // 429 is transient — delivery handler will Retry (inst-dlv-6)
    let server = MockServer::start();
    let _mock = server.mock(|when, then| {
        when.method(POST).path("/usage-collector/v1/records");
        then.status(429).body("quota-exceeded-marker");
    });

    let client = make_client(&server.base_url(), MockAuthN::with_token("tok"));
    let err = client.create_usage_record(test_record()).await.unwrap_err();
    let UsageCollectorError::ResourceExhausted { ctx, .. } = &err else {
        panic!("expected ResourceExhausted, got: {err:?}");
    };
    // The quota_violation payload must capture the body so operators can see
    // which limit was hit — a regression that drops it would leave only the
    // hard-coded "rate limit exceeded" string with no actionable detail.
    assert_eq!(ctx.violations.len(), 1);
    assert_eq!(ctx.violations[0].subject, "requests");
    assert!(
        ctx.violations[0]
            .description
            .contains("quota-exceeded-marker"),
        "quota violation description must contain a marker from the response body, got: {}",
        ctx.violations[0].description
    );
}

#[tokio::test]
async fn create_usage_record_server_500_returns_service_unavailable() {
    // 500 is transient — delivery handler will Retry (inst-dlv-6)
    let server = MockServer::start();
    let _mock = server.mock(|when, then| {
        when.method(POST).path("/usage-collector/v1/records");
        then.status(500).body("server-error-marker");
    });

    let client = make_client(&server.base_url(), MockAuthN::with_token("tok"));
    let err = client.create_usage_record(test_record()).await.unwrap_err();
    assert!(matches!(
        err,
        UsageCollectorError::ServiceUnavailable { .. }
    ));
    let msg = err.to_string();
    assert!(
        msg.contains("500") && msg.contains("server-error-marker"),
        "Display must include the HTTP status and a marker from the response body, got: {msg}"
    );
}

#[tokio::test]
async fn create_usage_record_server_500_with_large_non_ascii_body_is_truncated_safely() {
    // 4-byte UTF-8 codepoint ("🦀"), repeated to comfortably exceed 4 KiB.
    const CRAB: &str = "\u{1F980}";

    // The error path reads up to 4 KiB of the response body for diagnostics
    // and must truncate on a UTF-8 char boundary so the resulting String
    // never panics or contains invalid UTF-8. Non-ASCII characters (here:
    // 4-byte emoji) ensure the byte boundary at MAX=4096 falls inside a
    // multi-byte sequence on at least one of them.
    let server = MockServer::start();
    let large_body: String = CRAB.repeat(2_000); // 2000 * 4 = 8000 bytes
    let _mock = server.mock(|when, then| {
        when.method(POST).path("/usage-collector/v1/records");
        then.status(500).body(&large_body);
    });

    let client = make_client(&server.base_url(), MockAuthN::with_token("tok"));
    let err = client.create_usage_record(test_record()).await.unwrap_err();
    let UsageCollectorError::ServiceUnavailable { detail, .. } = &err else {
        panic!("expected ServiceUnavailable, got: {err:?}");
    };

    // The detail string contains a fixed prefix ("usage collector returned HTTP 500: ")
    // followed by the truncated body. Pin the body portion to the 4 KiB cap rather
    // than the original 8 000-byte upper bound, which gave the truncation a 2x slack
    // it never needed.
    let prefix = format!(
        "usage collector returned HTTP {}: ",
        StatusCode::from_u16(500).unwrap()
    );
    let body_part = detail.strip_prefix(&prefix).unwrap_or_else(|| {
        panic!("detail must start with the HTTP-status prefix {prefix:?}, got: {detail:?}")
    });
    assert!(
        body_part.len() <= 4_096,
        "body portion must be <= 4 KiB cap, got {} bytes",
        body_part.len()
    );
    // The truncation must respect UTF-8 char boundaries: the body portion
    // must consist of whole crab emojis only, with at least one present.
    assert!(
        body_part.contains(CRAB),
        "truncated body must still contain at least one full crab codepoint"
    );
    assert!(
        body_part.chars().all(|c| c.to_string() == CRAB),
        "truncated body must not contain partial or replacement codepoints"
    );
}

#[tokio::test]
async fn create_usage_record_server_422_returns_invalid_argument() {
    // 422 specifically signals schema/validation failure of the submitted
    // record — semantically distinct from "Internal" (which is what the
    // generic 4xx catch-all returns). Operators triaging a stuck record
    // benefit from the explicit InvalidArgument class.
    let server = MockServer::start();
    let _mock = server.mock(|when, then| {
        when.method(POST).path("/usage-collector/v1/records");
        then.status(422).body("unprocessable-marker");
    });

    let client = make_client(&server.base_url(), MockAuthN::with_token("tok"));
    let err = client.create_usage_record(test_record()).await.unwrap_err();
    let UsageCollectorError::InvalidArgument { ctx, .. } = &err else {
        panic!("expected InvalidArgument, got: {err:?}");
    };
    let serialized = serde_json::to_string(ctx).unwrap();
    assert!(
        serialized.contains("422") && serialized.contains("unprocessable-marker"),
        "InvalidArgument context must include the HTTP status and a marker from the response body, got: {serialized}",
    );
}

#[tokio::test]
async fn create_usage_record_server_400_returns_internal() {
    // Unexpected 4xx is permanent
    let server = MockServer::start();
    let _mock = server.mock(|when, then| {
        when.method(POST).path("/usage-collector/v1/records");
        then.status(400).body("bad-request-marker");
    });

    let client = make_client(&server.base_url(), MockAuthN::with_token("tok"));
    let err = client.create_usage_record(test_record()).await.unwrap_err();
    // Internal errors deliberately hide their detail from Display (the canonical
    // envelope serves a redacted message to clients). The original status and
    // body live in `ctx.description`, which operators see via tracing.
    let UsageCollectorError::Internal { ctx, .. } = &err else {
        panic!("expected Internal, got: {err:?}");
    };
    assert!(
        ctx.description.contains("400") && ctx.description.contains("bad-request-marker"),
        "Internal description must include the HTTP status and a marker from the response body, got: {}",
        ctx.description
    );
}

#[tokio::test]
async fn create_usage_record_base_url_trailing_slash_root_path_is_used() {
    // With no path prefix, a trailing slash on collector_url must not produce
    // a doubled '/' or an extra empty segment — the request should still hit
    // exactly `/usage-collector/v1/records` at the origin root.
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST).path("/usage-collector/v1/records");
        then.status(204);
    });

    let url_with_slash = format!("{}/", server.base_url());
    let client = make_client(&url_with_slash, MockAuthN::with_token("tok"));
    assert!(client.create_usage_record(test_record()).await.is_ok());
    mock.assert();
}

#[tokio::test]
async fn create_usage_record_preserves_collector_url_path_prefix() {
    // Operators may configure a collector URL that already includes a path
    // prefix (e.g. behind an ingress at https://gw/usage/). The API segments
    // must be appended to that prefix rather than wiping it.
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST).path("/usage/usage-collector/v1/records");
        then.status(204);
    });

    let url_with_prefix = format!("{}/usage/", server.base_url());
    let client = make_client(&url_with_prefix, MockAuthN::with_token("tok"));
    assert!(client.create_usage_record(test_record()).await.is_ok());
    mock.assert();
}

#[tokio::test]
async fn get_module_config_preserves_collector_url_path_prefix() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(GET)
            .path("/usage/usage-collector/v1/modules/mod-x/config");
        then.status(200)
            .json_body(json!({"allowed_metrics": [], "max_metadata_bytes": 0}));
    });

    let url_with_prefix = format!("{}/usage/", server.base_url());
    let client = make_client(&url_with_prefix, MockAuthN::with_token("tok"));
    client.get_module_config("mod-x").await.unwrap();
    mock.assert();
}

#[tokio::test]
async fn create_usage_record_sends_subject_fields_when_present() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/usage-collector/v1/records")
            .body_includes("\"subject\"")
            .body_includes("\"type\":\"test.subject\"");
        then.status(204);
    });

    let record = UsageRecord {
        subject: Some(Subject::with_type(Uuid::nil(), "test.subject")),
        ..test_record()
    };
    let client = make_client(&server.base_url(), MockAuthN::with_token("tok"));
    client.create_usage_record(record).await.unwrap();
    mock.assert();
}

#[tokio::test]
async fn create_usage_record_omits_subject_fields_when_absent() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/usage-collector/v1/records")
            .body_excludes("\"subject\"");
        then.status(204);
    });

    let record = UsageRecord {
        subject: None,
        ..test_record()
    };
    let client = make_client(&server.base_url(), MockAuthN::with_token("tok"));
    client.create_usage_record(record).await.unwrap();
    mock.assert();
}

#[tokio::test]
async fn get_module_config_success() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(GET)
            .path("/usage-collector/v1/modules/my-module/config");
        then.status(200)
            .json_body(json!({"allowed_metrics": [], "max_metadata_bytes": 0}));
    });

    let client = make_client(&server.base_url(), MockAuthN::with_token("tok"));
    let cfg = client.get_module_config("my-module").await.unwrap();
    assert!(cfg.allowed_metrics.is_empty());
    mock.assert();
}

#[tokio::test]
async fn get_module_config_sends_bearer_token_header() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(GET)
            .path("/usage-collector/v1/modules/mod-x/config")
            .header("authorization", "Bearer cfg-token");
        then.status(200)
            .json_body(json!({"allowed_metrics": [], "max_metadata_bytes": 0}));
    });

    let client = make_client(&server.base_url(), MockAuthN::with_token("cfg-token"));
    client.get_module_config("mod-x").await.unwrap();
    mock.assert();
}

#[tokio::test]
async fn get_module_config_server_401_returns_service_unavailable() {
    // 401 is transient — expired token; next attempt acquires a fresh bearer token (inst-rem-9)
    let server = MockServer::start();
    let _mock = server.mock(|when, then| {
        when.method(GET)
            .path("/usage-collector/v1/modules/mod-x/config");
        then.status(401).body("cfg-token-expired-marker");
    });

    let client = make_client(&server.base_url(), MockAuthN::with_token("tok"));
    let err = client.get_module_config("mod-x").await.unwrap_err();
    assert!(matches!(
        err,
        UsageCollectorError::ServiceUnavailable { .. }
    ));
    let msg = err.to_string();
    assert!(
        msg.contains("401") && msg.contains("cfg-token-expired-marker"),
        "Display must include the HTTP status and a marker from the response body, got: {msg}"
    );
}

#[tokio::test]
async fn get_module_config_server_404_returns_not_found() {
    let server = MockServer::start();
    let _mock = server.mock(|when, then| {
        when.method(GET)
            .path("/usage-collector/v1/modules/unknown-mod/config");
        then.status(404);
    });

    let client = make_client(&server.base_url(), MockAuthN::with_token("tok"));
    let err = client.get_module_config("unknown-mod").await.unwrap_err();
    // The queried module_name must surface on the NotFound variant — both as
    // `resource_name` (consumed by the canonical-error envelope) and in
    // `detail` (operator diagnostics). A regression that loses either would
    // still satisfy a bare `matches!` check on the variant.
    let UsageCollectorError::NotFound {
        detail,
        resource_name,
        ..
    } = &err
    else {
        panic!("expected NotFound, got: {err:?}");
    };
    assert_eq!(
        resource_name.as_deref(),
        Some("unknown-mod"),
        "NotFound resource_name must echo the queried module_name"
    );
    assert!(
        detail.contains("unknown-mod"),
        "NotFound detail must reference the queried module_name, got: {detail}"
    );
}

#[tokio::test]
async fn get_module_config_server_403_returns_permission_denied() {
    let server = MockServer::start();
    let _mock = server.mock(|when, then| {
        when.method(GET)
            .path("/usage-collector/v1/modules/mod-x/config");
        then.status(403).body("cfg-forbidden-marker");
    });

    let client = make_client(&server.base_url(), MockAuthN::with_token("tok"));
    let err = client.get_module_config("mod-x").await.unwrap_err();
    // Symmetric with the records-path 403 test: the PDP-denial reason must
    // carry the upstream body so operators can see which policy fired.
    let UsageCollectorError::PermissionDenied { ctx, .. } = &err else {
        panic!("expected PermissionDenied, got: {err:?}");
    };
    assert!(
        ctx.reason.contains("403") && ctx.reason.contains("cfg-forbidden-marker"),
        "PermissionDenied reason must include the HTTP status and a marker from the response body, got: {}",
        ctx.reason
    );
}

#[tokio::test]
async fn get_module_config_server_429_returns_resource_exhausted() {
    let server = MockServer::start();
    let _mock = server.mock(|when, then| {
        when.method(GET)
            .path("/usage-collector/v1/modules/mod-x/config");
        then.status(429).body("cfg-quota-exceeded-marker");
    });

    let client = make_client(&server.base_url(), MockAuthN::with_token("tok"));
    let err = client.get_module_config("mod-x").await.unwrap_err();
    let UsageCollectorError::ResourceExhausted { ctx, .. } = &err else {
        panic!("expected ResourceExhausted, got: {err:?}");
    };
    assert_eq!(ctx.violations.len(), 1);
    assert_eq!(ctx.violations[0].subject, "requests");
    assert!(
        ctx.violations[0]
            .description
            .contains("cfg-quota-exceeded-marker"),
        "quota violation description must contain a marker from the response body, got: {}",
        ctx.violations[0].description
    );
}

#[tokio::test]
async fn get_module_config_server_500_returns_service_unavailable() {
    // 500 from the config endpoint is transient
    let server = MockServer::start();
    let _mock = server.mock(|when, then| {
        when.method(GET)
            .path("/usage-collector/v1/modules/mod-x/config");
        then.status(500).body("cfg-server-error-marker");
    });

    let client = make_client(&server.base_url(), MockAuthN::with_token("tok"));
    let err = client.get_module_config("mod-x").await.unwrap_err();
    assert!(matches!(
        err,
        UsageCollectorError::ServiceUnavailable { .. }
    ));
    let msg = err.to_string();
    assert!(
        msg.contains("500") && msg.contains("cfg-server-error-marker"),
        "Display must include the HTTP status and a marker from the response body, got: {msg}"
    );
}

#[tokio::test]
async fn get_module_config_server_400_returns_internal() {
    // Unexpected 4xx is permanent
    let server = MockServer::start();
    let _mock = server.mock(|when, then| {
        when.method(GET)
            .path("/usage-collector/v1/modules/mod-x/config");
        then.status(400).body("cfg-bad-request-marker");
    });

    let client = make_client(&server.base_url(), MockAuthN::with_token("tok"));
    let err = client.get_module_config("mod-x").await.unwrap_err();
    // See create_usage_record_server_400_returns_internal — Internal hides
    // the detail from Display; the marker lives in `ctx.description`.
    let UsageCollectorError::Internal { ctx, .. } = &err else {
        panic!("expected Internal, got: {err:?}");
    };
    assert!(
        ctx.description.contains("400") && ctx.description.contains("cfg-bad-request-marker"),
        "Internal description must include the HTTP status and a marker from the response body, got: {}",
        ctx.description
    );
}

#[tokio::test]
async fn get_module_config_invalid_json_response_returns_internal() {
    let server = MockServer::start();
    let _mock = server.mock(|when, then| {
        when.method(GET)
            .path("/usage-collector/v1/modules/mod-x/config");
        then.status(200).body("not-json");
    });

    let client = make_client(&server.base_url(), MockAuthN::with_token("tok"));
    let err = client.get_module_config("mod-x").await.unwrap_err();
    // The interesting behavior is that the JSON parse error is preserved in
    // `ctx.description` for diagnostics — a regression that mapped parse errors
    // to a generic empty `Internal` would still satisfy a bare `matches!` check.
    let UsageCollectorError::Internal { ctx, .. } = &err else {
        panic!("expected Internal, got: {err:?}");
    };
    assert!(
        ctx.description
            .contains("failed to parse module config response"),
        "Internal description must include the parse-error marker, got: {}",
        ctx.description
    );
}

#[tokio::test]
async fn get_module_config_returns_allowed_metrics() {
    let server = MockServer::start();
    let _mock = server.mock(|when, then| {
        when.method(GET)
            .path("/usage-collector/v1/modules/my-mod/config");
        then.status(200).json_body(json!({
            "allowed_metrics": [
                {"name": "cpu.usage", "kind": "gauge"},
                {"name": "req.count", "kind": "counter"}
            ],
            "max_metadata_bytes": 0
        }));
    });

    let client = make_client(&server.base_url(), MockAuthN::with_token("tok"));
    let ModuleConfig {
        allowed_metrics,
        max_metadata_bytes: _,
    } = client.get_module_config("my-mod").await.unwrap();
    assert_eq!(allowed_metrics.len(), 2);
    assert_eq!(allowed_metrics[0].name, "cpu.usage");
    assert_eq!(allowed_metrics[0].kind, UsageKind::Gauge);
    assert_eq!(allowed_metrics[1].name, "req.count");
    assert_eq!(allowed_metrics[1].kind, UsageKind::Counter);
}

// inst-cfg-rem-3: percent-encoding of module_name in get_module_config URL path

#[tokio::test]
async fn get_module_config_percent_encodes_slash_in_module_name() {
    // inst-cfg-rem-3
    // A module_name containing a '/' MUST be percent-encoded in the URL path
    // segment so the raw '/' does not appear unencoded, and the server receives
    // the encoded form '%2F' rather than an extra path separator.
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(GET)
            .path("/usage-collector/v1/modules/my%2Fmodule/config");
        then.status(200)
            .json_body(json!({"allowed_metrics": [], "max_metadata_bytes": 0}));
    });

    let client = make_client(&server.base_url(), MockAuthN::with_token("tok"));
    let result = client.get_module_config("my/module").await;
    assert!(
        result.is_ok(),
        "expected Ok but got Err: {result:?} — the mock server only matches '%2F', \
         so a failure here means the slash was not percent-encoded"
    );
    mock.assert();
}

#[tokio::test]
async fn get_module_config_percent_encodes_space_in_module_name() {
    // inst-cfg-rem-3
    // A module_name containing a space MUST be percent-encoded ('%20') in the
    // URL path segment.
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(GET)
            .path("/usage-collector/v1/modules/my%20module/config");
        then.status(200)
            .json_body(json!({"allowed_metrics": [], "max_metadata_bytes": 0}));
    });

    let client = make_client(&server.base_url(), MockAuthN::with_token("tok"));
    let result = client.get_module_config("my module").await;
    assert!(
        result.is_ok(),
        "expected Ok but got Err: {result:?} — the mock only matches '%20', \
         so a failure means the space was not percent-encoded"
    );
    mock.assert();
}

// Two representative AuthN-failure cases on the module-config path.
// See the create_usage_record_authn_* tests for rationale; the remaining
// AuthN variants are covered by `bearer_token_auth_layer_tests`.

#[tokio::test]
async fn get_module_config_authn_unauthorized_returns_service_unavailable() {
    let server = MockServer::start();
    let client = make_client(&server.base_url(), MockAuthN::unauthorized());

    let err = client.get_module_config("mod-x").await.unwrap_err();
    assert!(matches!(
        err,
        UsageCollectorError::ServiceUnavailable { .. }
    ));
    let msg = err.to_string();
    assert!(
        msg.contains("unauthorized") && msg.contains("bad credentials"),
        "Display must preserve the AuthN cause, got: {msg}"
    );
}

#[tokio::test]
async fn get_module_config_authn_service_unavailable_returns_service_unavailable() {
    // ServiceUnavailable is transient: the identity service is temporarily unreachable.
    let server = MockServer::start();
    let client = make_client(&server.base_url(), MockAuthN::service_unavailable());

    let err = client.get_module_config("mod-x").await.unwrap_err();
    assert!(matches!(
        err,
        UsageCollectorError::ServiceUnavailable { .. }
    ));
    let msg = err.to_string();
    assert!(
        msg.contains("service unavailable")
            && msg.contains("identity service temporarily unreachable"),
        "Display must preserve the AuthN cause, got: {msg}"
    );
}

// --- Unit tests for the transport-error → UsageCollectorError mapping helper ---
//
// The transport-error classifier is shared between `create_usage_record` and
// `get_module_config`, so the production code path that materialises e.g. a
// `HttpError::Timeout` from the underlying tower stack is the same — the only
// per-endpoint difference is the resource-scoped `deadline_exceeded` factory.
// Driving the helper directly with synthesised `HttpError` values is far more
// reliable than racing a real-clock timeout through `httpmock`, and keeps the
// load-bearing `inst-rem-6` arm covered against accidental regressions.

#[test]
fn map_transport_err_timeout_uses_records_deadline_factory() {
    // inst-rem-6: Timeout must classify as DeadlineExceeded so the delivery
    // handler retries instead of dead-lettering on a transient stall.
    let err = map_transport_err(HttpError::Timeout(Duration::from_millis(50)), |msg| {
        UsageRecordError::deadline_exceeded(msg).create()
    });
    let UsageCollectorError::DeadlineExceeded { resource_type, .. } = &err else {
        panic!("expected UsageCollectorError::DeadlineExceeded, got: {err:?}");
    };
    // Resource type comes from `UsageRecordError`'s GTS prefix, anchoring the
    // mapping to the records endpoint rather than the modules endpoint.
    let rt = resource_type
        .as_deref()
        .expect("resource-scoped DeadlineExceeded must carry a resource_type");
    assert!(
        rt.contains("usage.record"),
        "DeadlineExceeded must be scoped to the usage-record resource type, got: {rt}"
    );
}

#[test]
fn map_transport_err_deadline_exceeded_uses_records_deadline_factory() {
    // The total-deadline variant (across all retries) must funnel through the
    // same factory as a per-attempt Timeout — otherwise a circuit breaker that
    // matches on `UsageCollectorError::DeadlineExceeded` would silently miss
    // exhausted-deadline failures on the records path.
    let err = map_transport_err(
        HttpError::DeadlineExceeded(Duration::from_millis(50)),
        |msg| UsageRecordError::deadline_exceeded(msg).create(),
    );
    assert!(
        matches!(err, UsageCollectorError::DeadlineExceeded { .. }),
        "expected DeadlineExceeded, got: {err:?}"
    );
}

#[test]
fn map_transport_err_timeout_uses_module_config_deadline_factory() {
    // Symmetric to the records-path test above: the config endpoint must
    // surface its own resource-scoped DeadlineExceeded so operators triaging
    // a stuck config fetch see the module_config resource type rather than
    // usage.record.
    let err = map_transport_err(HttpError::Timeout(Duration::from_millis(50)), |msg| {
        ModuleConfigError::deadline_exceeded(msg).create()
    });
    let UsageCollectorError::DeadlineExceeded { resource_type, .. } = &err else {
        panic!("expected UsageCollectorError::DeadlineExceeded, got: {err:?}");
    };
    let rt = resource_type
        .as_deref()
        .expect("resource-scoped DeadlineExceeded must carry a resource_type");
    assert!(
        rt.contains("module_config"),
        "DeadlineExceeded must be scoped to the module_config resource type, got: {rt}"
    );
}

#[test]
fn map_transport_err_deadline_exceeded_uses_module_config_deadline_factory() {
    let err = map_transport_err(
        HttpError::DeadlineExceeded(Duration::from_millis(50)),
        |msg| ModuleConfigError::deadline_exceeded(msg).create(),
    );
    assert!(
        matches!(err, UsageCollectorError::DeadlineExceeded { .. }),
        "expected DeadlineExceeded, got: {err:?}"
    );
}
