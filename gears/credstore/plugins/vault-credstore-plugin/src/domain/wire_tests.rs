use reqwest::StatusCode;

use super::*;

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
fn decode_value_rejects_invalid_base64() {
    let err = decode_value("not-valid-base64!!").unwrap_err();
    assert!(matches!(err, CredStoreError::Internal(_)));
}

#[test]
fn decode_value_error_never_echoes_payload() {
    let bogus = "not-valid-base64!!";
    let err = decode_value(bogus).unwrap_err();
    assert!(!err.to_string().contains(bogus));
}

#[test]
fn put_request_body_serializes_cas_zero() {
    let body = PutRequestBody::create_only("aGVsbG8=".to_owned());
    let json = serde_json::to_string(&body).expect("serialize");
    assert_eq!(json, r#"{"options":{"cas":0},"data":{"value":"aGVsbG8="}}"#);
}

#[test]
fn parse_get_body_extracts_and_decodes_value() {
    let body = r#"{"data":{"data":{"value":"aGVsbG8="},"metadata":{"version":1}}}"#;
    let bytes = parse_get_body(body).expect("parses");
    assert_eq!(bytes, b"hello");
}

#[test]
fn parse_get_body_rejects_unexpected_shape() {
    let body = r#"{"not":"the expected shape"}"#;
    let err = parse_get_body(body).unwrap_err();
    assert!(matches!(err, CredStoreError::Internal(_)));
}

#[test]
fn is_cas_conflict_body_matches_vault_wire_format() {
    let body = r#"{"errors":["check-and-set parameter did not match the current version"]}"#;
    assert!(is_cas_conflict_body(body));
}

#[test]
fn is_cas_conflict_body_is_case_insensitive() {
    let body = r#"{"errors":["Check-And-Set parameter did not match"]}"#;
    assert!(is_cas_conflict_body(body));
}

#[test]
fn is_cas_conflict_body_rejects_unrelated_errors() {
    let body = r#"{"errors":["permission denied"]}"#;
    assert!(!is_cas_conflict_body(body));
}

#[test]
fn classify_get_response_404_is_none() {
    let got = classify_get_response(StatusCode::NOT_FOUND, "").expect("ok");
    assert!(got.is_none());
}

#[test]
fn classify_get_response_200_decodes_value() {
    let body = r#"{"data":{"data":{"value":"aGVsbG8="}}}"#;
    let got = classify_get_response(StatusCode::OK, body).expect("ok");
    assert_eq!(got, Some(b"hello".to_vec()));
}

#[test]
fn classify_get_response_5xx_is_unavailable() {
    let err = classify_get_response(StatusCode::INTERNAL_SERVER_ERROR, "").unwrap_err();
    assert!(err.is_unavailable());
}

#[test]
fn classify_get_response_403_is_internal_not_unavailable() {
    let err = classify_get_response(StatusCode::FORBIDDEN, "").unwrap_err();
    assert!(matches!(err, CredStoreError::Internal(_)));
    assert!(!err.is_unavailable());
}

#[test]
fn classify_put_response_2xx_is_ok() {
    classify_put_response(StatusCode::OK, "").expect("ok");
    classify_put_response(StatusCode::NO_CONTENT, "").expect("ok");
}

#[test]
fn classify_put_response_cas_mismatch_is_conflict() {
    let body = r#"{"errors":["check-and-set parameter did not match the current version"]}"#;
    let err = classify_put_response(StatusCode::BAD_REQUEST, body).unwrap_err();
    assert!(matches!(err, CredStoreError::Conflict));
}

#[test]
fn classify_put_response_other_400_is_not_conflict() {
    let body = r#"{"errors":["missing client token"]}"#;
    let err = classify_put_response(StatusCode::BAD_REQUEST, body).unwrap_err();
    assert!(!matches!(err, CredStoreError::Conflict));
}

#[test]
fn classify_put_response_5xx_is_unavailable() {
    let err = classify_put_response(StatusCode::SERVICE_UNAVAILABLE, "").unwrap_err();
    assert!(err.is_unavailable());
}

#[test]
fn classify_delete_response_204_is_ok() {
    classify_delete_response(StatusCode::NO_CONTENT).expect("ok");
}

#[test]
fn classify_delete_response_404_is_ok_idempotent() {
    classify_delete_response(StatusCode::NOT_FOUND).expect("ok, idempotent delete");
}

#[test]
fn classify_delete_response_5xx_is_unavailable() {
    let err = classify_delete_response(StatusCode::BAD_GATEWAY).unwrap_err();
    assert!(err.is_unavailable());
}
