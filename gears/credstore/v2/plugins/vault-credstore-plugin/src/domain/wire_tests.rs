// Created: 2026-09-23 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
use super::*;
use crate::domain::test_ext::PluginErrorExt as _;

/// What Vault answers (dev server) for a soft-deleted version: `404`, no
/// `errors`, `data.data = null`, `deletion_time` set.
const SOFT_DELETED_404: &str = r#"{"request_id":"r","lease_id":"","renewable":false,"lease_duration":0,"data":{"data":null,"metadata":{"created_time":"2026-10-03T21:22:01.155040009Z","custom_metadata":null,"deletion_time":"2026-10-03T21:22:01.220348343Z","destroyed":false,"version":2}},"wrap_info":null,"warnings":null,"auth":null,"mount_type":""}"#;

/// What Vault answers for a destroyed version: `404`, `data.data = null`,
/// `destroyed = true`.
const DESTROYED_404: &str = r#"{"request_id":"r","lease_id":"","renewable":false,"lease_duration":0,"data":{"data":null,"metadata":{"created_time":"2026-10-03T21:22:01.141165509Z","custom_metadata":null,"deletion_time":"","destroyed":true,"version":1}},"wrap_info":null,"warnings":null,"auth":null,"mount_type":""}"#;

/// What Vault answers for an absent key or version.
const ABSENT_404: &str = r#"{"errors":[]}"#;

/// What Vault answers for a path under a mount that does not exist.
const NO_MOUNT_404: &str =
    r#"{"errors":["no handler for route \"nomount/data/x/y\". route entry not found."]}"#;

/// What Vault answers for a missing or revoked token.
const FORBIDDEN_403: &str =
    r#"{"errors":["2 errors occurred:\n\t* permission denied\n\t* invalid token\n\n"]}"#;

fn live_body(value_b64: &str) -> String {
    format!(
        r#"{{"data":{{"data":{{"value":"{value_b64}"}},"metadata":{{"created_time":"t","custom_metadata":null,"deletion_time":"","destroyed":false,"version":2}}}}}}"#
    )
}

#[test]
fn data_path_matches_kv_v2_shape() {
    assert_eq!(
        data_path("secret", "credstore", "tenant-1", "value-1"),
        "secret/data/credstore/tenant-1/value-1"
    );
}

#[test]
fn metadata_path_matches_kv_v2_shape() {
    assert_eq!(
        metadata_path("secret", "credstore", "tenant-1", "value-1"),
        "secret/metadata/credstore/tenant-1/value-1"
    );
}

#[test]
fn destroy_path_matches_kv_v2_shape() {
    assert_eq!(
        destroy_path("secret", "credstore", "tenant-1", "rec-1"),
        "secret/destroy/credstore/tenant-1/rec-1"
    );
}

#[test]
fn full_url_joins_address_and_path() {
    assert_eq!(
        full_url("http://127.0.0.1:8200", "secret/data/credstore/t/v"),
        "http://127.0.0.1:8200/v1/secret/data/credstore/t/v"
    );
}

#[test]
fn full_url_tolerates_trailing_slash_on_address() {
    assert_eq!(
        full_url("http://127.0.0.1:8200/", "secret/data/credstore/t/v"),
        "http://127.0.0.1:8200/v1/secret/data/credstore/t/v"
    );
}

#[test]
fn encode_decode_value_round_trips() {
    let bytes = b"hello-from-openbao".to_vec();
    let encoded = encode_value(&bytes);
    assert_eq!(decode_value(&encoded).expect("valid base64"), bytes);
}

#[test]
fn decode_value_rejects_invalid_base64_as_internal() {
    let err = decode_value("not-valid-base64!!").unwrap_err();
    assert!(matches!(err, CredStorePluginError::Internal { .. }));
}

#[test]
fn decode_value_error_never_echoes_payload() {
    let bogus = "not-valid-base64!!";
    let err = decode_value(bogus).unwrap_err();
    assert!(!err.to_string().contains(bogus));
}

#[test]
fn put_request_body_has_no_cas() {
    let body = PutRequestBody::new("aGVsbG8=");
    let json = serde_json::to_string(&body).expect("serialize");
    assert_eq!(json, r#"{"data":{"value":"aGVsbG8="}}"#);
}

#[test]
fn destroy_request_body_lists_versions() {
    let json = to_json_body(&DestroyRequestBody {
        versions: vec![2, 3],
    })
    .expect("ok");
    assert_eq!(json, r#"{"versions":[2,3]}"#);
}

// -- read bodies ---------------------------------------------------------------

#[test]
fn parse_get_body_extracts_and_decodes_value() {
    let bytes = parse_get_body(&live_body("aGVsbG8=")).expect("parses");
    assert_eq!(bytes, Some(b"hello".to_vec()));
}

#[test]
fn parse_get_body_without_metadata_still_decodes() {
    let body = r#"{"data":{"data":{"value":"aGVsbG8="}}}"#;
    assert_eq!(
        parse_get_body(body).expect("parses"),
        Some(b"hello".to_vec())
    );
}

#[test]
fn parse_get_body_empty_value_is_a_value() {
    assert_eq!(
        parse_get_body(&live_body("")).expect("parses"),
        Some(Vec::new())
    );
}

#[test]
fn parse_get_body_null_data_is_none() {
    let body = r#"{"data":{"data":null,"metadata":{"version":1}}}"#;
    assert_eq!(parse_get_body(body).expect("parses"), None);
}

#[test]
fn parse_get_body_soft_deleted_and_destroyed_are_none() {
    assert_eq!(parse_get_body(SOFT_DELETED_404).expect("parses"), None);
    assert_eq!(parse_get_body(DESTROYED_404).expect("parses"), None);
}

#[test]
fn parse_get_body_value_with_deletion_metadata_is_none() {
    // Not seen from a real Vault (it answers 404), but a version that is
    // marked deleted must never be served.
    let deleted = r#"{"data":{"data":{"value":"aGVsbG8="},"metadata":{"deletion_time":"2026-10-03T21:22:01Z","destroyed":false}}}"#;
    assert_eq!(parse_get_body(deleted).expect("parses"), None);
    let destroyed = r#"{"data":{"data":{"value":"aGVsbG8="},"metadata":{"deletion_time":"","destroyed":true}}}"#;
    assert_eq!(parse_get_body(destroyed).expect("parses"), None);
    let null_time = r#"{"data":{"data":{"value":"aGVsbG8="},"metadata":{"deletion_time":null,"destroyed":false}}}"#;
    assert_eq!(
        parse_get_body(null_time).expect("parses"),
        Some(b"hello".to_vec())
    );
}

#[test]
fn parse_get_body_entry_without_a_value_is_internal() {
    for body in [
        r#"{"data":{"data":{"password":"x"}}}"#,
        r#"{"data":{"data":{"value":7}}}"#,
        r#"{"data":{"data":{}}}"#,
        r#"{"data":{"data":"text"}}"#,
        r#"{"data":{"data":{"value":"not base64!!"}}}"#,
    ] {
        assert!(
            matches!(
                parse_get_body(body),
                Err(CredStorePluginError::Internal { .. })
            ),
            "{body}"
        );
    }
}

#[test]
fn parse_get_body_rejects_unexpected_shape_as_internal() {
    for body in [
        r#"{"not":"the expected shape"}"#,
        "",
        "<html>bad gateway</html>",
    ] {
        let err = parse_get_body(body).unwrap_err();
        assert!(
            matches!(err, CredStorePluginError::Internal { .. }),
            "{body}"
        );
    }
}

#[test]
fn parse_get_body_error_never_echoes_the_body() {
    let err = parse_get_body(r#"{"data":"c2VjcmV0"}"#).unwrap_err();
    assert!(!err.to_string().contains("c2VjcmV0"));
}

#[test]
fn parse_put_body_returns_version_as_string() {
    let body = r#"{"data":{"version":7,"created_time":"x"}}"#;
    assert_eq!(parse_put_body(body).expect("parses"), "7");
    assert!(parse_put_body("{}").is_err());
    assert!(parse_put_body("").is_err());
}

#[test]
fn parse_live_versions_skips_destroyed_and_sorts() {
    let body = r#"{"data":{"versions":{
        "3":{"destroyed":false},"1":{"destroyed":true},"2":{"destroyed":false,"deletion_time":"t"}}}}"#;
    assert_eq!(parse_live_versions(body).expect("parses"), vec![2, 3]);
}

#[test]
fn parse_live_versions_sorts_numerically_and_ignores_junk_keys() {
    let body = r#"{"data":{"versions":{
        "10":{"destroyed":false},"9":{"destroyed":false},"2":{"destroyed":false},"x":{"destroyed":false}}}}"#;
    assert_eq!(parse_live_versions(body).expect("parses"), vec![2, 9, 10]);
}

#[test]
fn parse_live_versions_of_a_real_metadata_body() {
    // Shape returned by a dev server: the oldest versions were evicted by
    // `max_versions`, one is destroyed, one soft-deleted.
    let body = r#"{"data":{"cas_required":false,"current_version":4,"max_versions":3,"oldest_version":2,"versions":{
        "2":{"created_by":{"actor":"token"},"deleted_by":null,"deletion_time":"","destroyed":true},
        "3":{"created_by":{"actor":"token"},"deleted_by":{"actor":"token"},"deletion_time":"2026-10-03T21:22:01Z","destroyed":false},
        "4":{"created_by":{"actor":"token"},"deleted_by":null,"deletion_time":"","destroyed":false}}}}"#;
    assert_eq!(parse_live_versions(body).expect("parses"), vec![3, 4]);
}

#[test]
fn parse_live_versions_rejects_unexpected_shape() {
    assert!(matches!(
        parse_live_versions("{}").unwrap_err(),
        CredStorePluginError::Internal { .. }
    ));
}

#[test]
fn parse_version_accepts_numbers_only() {
    assert_eq!(parse_version("12").expect("ok"), 12);
    assert_eq!(parse_version("0").expect("ok"), 0);
    assert!(parse_version("abc").is_err());
    assert!(parse_version("").is_err());
    assert!(parse_version("-1").is_err());
}

// -- error text ----------------------------------------------------------------

#[test]
fn error_text_joins_vault_errors_and_flattens_whitespace() {
    assert_eq!(error_text(r#"{"errors":["a","b"]}"#, &[]), "a; b");
    assert_eq!(
        error_text(FORBIDDEN_403, &[]),
        "2 errors occurred: * permission denied * invalid token"
    );
}

#[test]
fn error_text_of_anything_else_is_empty() {
    for body in ["", "not json", "{}", ABSENT_404, r#"{"errors":"x"}"#] {
        assert_eq!(error_text(body, &[]), "", "{body:?}");
    }
}

#[test]
fn error_text_is_cut_to_200_chars() {
    let long = "x".repeat(500);
    let body = format!(r#"{{"errors":["{long}"]}}"#);
    assert_eq!(error_text(&body, &[]).chars().count(), 200);
}

#[test]
fn error_text_redacts_the_given_secrets() {
    let body = r#"{"errors":["bad value c2VjcmV0LXZhbHVl in request"]}"#;
    let text = error_text(body, &["c2VjcmV0LXZhbHVl", ""]);
    assert_eq!(text, "bad value <redacted> in request");
}

#[test]
fn is_transient_status_covers_5xx_408_429_only() {
    for status in [408, 429, 500, 502, 503, 504, 599] {
        assert!(is_transient_status(status), "{status}");
    }
    for status in [200, 204, 300, 400, 403, 404, 409, 412, 499, 600] {
        assert!(!is_transient_status(status), "{status}");
    }
}

// -- error mapping table ---------------------------------------------------------

#[test]
fn transient_statuses_map_to_unavailable() {
    for status in [408, 429, 500, 502, 503, 599] {
        let err = map_error_status(status, "", None, &[]);
        assert!(err.is_unavailable(), "{status}");
        assert_eq!(err.retry_after().map(|d| d.as_secs()), None);
    }
}

#[test]
fn retry_after_is_passed_on() {
    let err = map_error_status(503, "", Some(Duration::from_secs(7)), &[]);
    assert_eq!(err.retry_after().map(|d| d.as_secs()), Some(7));
}

#[test]
fn unavailable_detail_carries_status_and_vault_text() {
    let err = map_error_status(503, r#"{"errors":["Vault is sealed"]}"#, None, &[]);
    let msg = err.to_string();
    assert!(msg.contains("503"), "{msg}");
    assert!(msg.contains("Vault is sealed"), "{msg}");
}

#[test]
fn forbidden_maps_to_credentials_rejected_without_leaking_anything() {
    let err = map_error_status(403, FORBIDDEN_403, None, &[]);
    assert!(err.is_credentials_rejected());
    assert_eq!(err.retry_after(), None);
    let msg = err.to_string();
    assert!(msg.contains("403"), "{msg}");
    assert!(!msg.contains("invalid token"), "{msg}");
}

#[test]
fn bad_request_maps_to_internal_with_vault_text() {
    let err = map_error_status(400, r#"{"errors":["no data provided"]}"#, None, &[]);
    assert!(
        matches!(&err, CredStorePluginError::Internal { detail: m, .. } if m.contains("no data provided"))
    );
}

#[test]
fn other_statuses_map_to_internal() {
    for status in [300, 301, 401, 404, 405, 409, 412, 499, 600] {
        let err = map_error_status(status, "", None, &[]);
        assert!(
            matches!(err, CredStorePluginError::Internal { .. }),
            "{status}"
        );
        assert!(!err.is_unavailable(), "{status}");
    }
}

#[test]
fn error_messages_never_contain_the_redacted_secret() {
    let body = r#"{"errors":["rejected c2VjcmV0"]}"#;
    for status in [400, 422, 500] {
        let msg = map_error_status(status, body, None, &["c2VjcmV0"]).to_string();
        assert!(!msg.contains("c2VjcmV0"), "{status}: {msg}");
    }
}

// -- classification --------------------------------------------------------------

#[test]
fn classify_get_response_absent_version_or_key_is_none() {
    assert_eq!(classify_get_response(404, ABSENT_404).expect("ok"), None);
    assert_eq!(classify_get_response(404, "").expect("ok"), None);
}

#[test]
fn classify_get_response_soft_deleted_and_destroyed_are_none() {
    assert_eq!(
        classify_get_response(404, SOFT_DELETED_404).expect("ok"),
        None
    );
    assert_eq!(classify_get_response(404, DESTROYED_404).expect("ok"), None);
}

#[test]
fn classify_get_response_missing_mount_is_an_error_not_a_miss() {
    let err = classify_get_response(404, NO_MOUNT_404).unwrap_err();
    assert!(
        matches!(&err, CredStorePluginError::Internal { detail: m, .. } if m.contains("no handler for route"))
    );
}

#[test]
fn classify_get_response_200_decodes_value() {
    let got = classify_get_response(200, &live_body("aGVsbG8=")).expect("ok");
    assert_eq!(got, Some(b"hello".to_vec()));
}

#[test]
fn classify_get_response_failures() {
    assert!(classify_get_response(500, "").unwrap_err().is_unavailable());
    assert!(classify_get_response(408, "").unwrap_err().is_unavailable());
    assert!(classify_get_response(429, "").unwrap_err().is_unavailable());
    assert!(
        classify_get_response(403, FORBIDDEN_403)
            .unwrap_err()
            .is_credentials_rejected()
    );
    assert!(matches!(
        classify_get_response(400, r#"{"errors":["x"]}"#).unwrap_err(),
        CredStorePluginError::Internal { .. }
    ));
}

#[test]
fn classify_get_response_status_boundaries() {
    // 2xx range ends at 299, 5xx range starts at 500.
    assert!(classify_get_response(299, r#"{"data":{"data":null}}"#).is_ok());
    assert!(matches!(
        classify_get_response(300, "").unwrap_err(),
        CredStorePluginError::Internal { .. }
    ));
    assert!(matches!(
        classify_get_response(499, "").unwrap_err(),
        CredStorePluginError::Internal { .. }
    ));
    assert!(classify_get_response(500, "").unwrap_err().is_unavailable());
    assert!(classify_get_response(599, "").unwrap_err().is_unavailable());
    assert!(matches!(
        classify_get_response(600, "").unwrap_err(),
        CredStorePluginError::Internal { .. }
    ));
}

#[test]
fn metadata_request_body_carries_only_max_versions() {
    let body = to_json_body(&MetadataRequestBody { max_versions: 100 }).expect("ok");
    assert_eq!(body, r#"{"max_versions":100}"#);
}

#[test]
fn classify_metadata_write_response_2xx_succeeds_and_the_rest_fails() {
    classify_metadata_write_response(204, "").expect("ok");
    classify_metadata_write_response(200, "{}").expect("ok");
    assert!(
        classify_metadata_write_response(503, "")
            .unwrap_err()
            .is_unavailable()
    );
    assert!(
        classify_metadata_write_response(403, FORBIDDEN_403)
            .unwrap_err()
            .is_credentials_rejected()
    );
    // Unlike a delete, a plain 404 is not success here.
    assert!(classify_metadata_write_response(404, "").is_err());
    assert!(classify_metadata_write_response(400, "{}").is_err());
}

#[test]
fn classify_put_response_2xx_returns_version() {
    let got = classify_put_response(200, r#"{"data":{"version":3}}"#, "aGk=").expect("ok");
    assert_eq!(got, "3");
}

#[test]
fn classify_put_response_failures() {
    assert!(
        classify_put_response(503, "", "aGk=")
            .unwrap_err()
            .is_unavailable()
    );
    assert!(
        classify_put_response(403, FORBIDDEN_403, "aGk=")
            .unwrap_err()
            .is_credentials_rejected()
    );
    assert!(matches!(
        classify_put_response(400, "{}", "aGk=").unwrap_err(),
        CredStorePluginError::Internal { .. }
    ));
    // A `404` on a write means the mount is missing: an error, never success.
    assert!(matches!(
        classify_put_response(404, NO_MOUNT_404, "aGk=").unwrap_err(),
        CredStorePluginError::Internal { .. }
    ));
}

#[test]
fn classify_put_response_400_text_never_echoes_the_secret() {
    let body = r#"{"errors":["invalid data c2VjcmV0"]}"#;
    let msg = classify_put_response(400, body, "c2VjcmV0")
        .unwrap_err()
        .to_string();
    assert!(msg.contains("invalid data"), "{msg}");
    assert!(!msg.contains("c2VjcmV0"), "{msg}");
}

#[test]
fn classify_metadata_response_404_is_empty() {
    assert!(
        classify_metadata_response(404, ABSENT_404)
            .expect("ok")
            .is_empty()
    );
    assert!(classify_metadata_response(404, "").expect("ok").is_empty());
    assert!(matches!(
        classify_metadata_response(404, NO_MOUNT_404).unwrap_err(),
        CredStorePluginError::Internal { .. }
    ));
}

#[test]
fn classify_metadata_response_lists_live_versions() {
    let body = r#"{"data":{"versions":{"1":{"destroyed":true},"2":{"destroyed":false}}}}"#;
    assert_eq!(classify_metadata_response(200, body).expect("ok"), vec![2]);
    assert!(
        classify_metadata_response(502, "")
            .unwrap_err()
            .is_unavailable()
    );
}

#[test]
fn classify_delete_and_destroy_responses() {
    for classify in [classify_delete_response, classify_destroy_response] {
        classify(204, "").expect("204 is ok");
        classify(200, "").expect("200 is ok");
        classify(404, ABSENT_404).expect("a missing key is ok, idempotent");
        classify(404, "").expect("a bare 404 is ok, idempotent");
        assert!(classify(502, "").unwrap_err().is_unavailable());
        assert!(
            classify(403, FORBIDDEN_403)
                .unwrap_err()
                .is_credentials_rejected()
        );
        assert!(matches!(
            classify(404, NO_MOUNT_404).unwrap_err(),
            CredStorePluginError::Internal { .. }
        ));
        assert!(matches!(
            classify(400, r#"{"errors":["no version number provided"]}"#).unwrap_err(),
            CredStorePluginError::Internal { .. }
        ));
    }
}

#[test]
fn to_json_body_serializes_the_request_shapes() {
    let put = to_json_body(&PutRequestBody::new("aGk=")).expect("ok");
    assert_eq!(put, r#"{"data":{"value":"aGk="}}"#);
}
