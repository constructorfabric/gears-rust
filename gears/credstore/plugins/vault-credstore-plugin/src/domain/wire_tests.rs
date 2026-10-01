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
fn destroy_path_matches_kv_v2_shape() {
    assert_eq!(
        destroy_path("secret", "credstore", "tenant-1", "rec-1"),
        "secret/destroy/credstore/tenant-1/rec-1"
    );
}

#[test]
fn put_request_body_has_no_cas() {
    let body = PutRequestBody::new("aGVsbG8=".to_owned());
    let json = serde_json::to_string(&body).expect("serialize");
    assert_eq!(json, r#"{"data":{"value":"aGVsbG8="}}"#);
}

#[test]
fn destroy_request_body_lists_versions() {
    let json = serde_json::to_string(&DestroyRequestBody {
        versions: vec![1, 2],
    })
    .expect("serialize");
    assert_eq!(json, r#"{"versions":[1,2]}"#);
}

#[test]
fn parse_get_body_extracts_and_decodes_value() {
    let body = r#"{"data":{"data":{"value":"aGVsbG8="},"metadata":{"version":1}}}"#;
    let bytes = parse_get_body(body).expect("parses");
    assert_eq!(bytes, Some(b"hello".to_vec()));
}

#[test]
fn parse_get_body_null_data_is_none() {
    let body = r#"{"data":{"data":null,"metadata":{"version":1,"deletion_time":"x"}}}"#;
    assert_eq!(parse_get_body(body).expect("parses"), None);
}

#[test]
fn parse_get_body_rejects_unexpected_shape() {
    let body = r#"{"not":"the expected shape"}"#;
    let err = parse_get_body(body).unwrap_err();
    assert!(matches!(err, CredStoreError::Internal(_)));
}

#[test]
fn parse_put_body_returns_version_as_string() {
    let body = r#"{"data":{"version":7,"created_time":"x"}}"#;
    assert_eq!(parse_put_body(body).expect("parses"), "7");
    assert!(parse_put_body("{}").is_err());
}

#[test]
fn parse_live_versions_skips_destroyed_and_sorts() {
    let body = r#"{"data":{"versions":{
        "3":{"destroyed":false},"1":{"destroyed":true},"2":{"destroyed":false,"deletion_time":"t"}}}}"#;
    assert_eq!(parse_live_versions(body).expect("parses"), vec![2, 3]);
}

#[test]
fn parse_version_accepts_numbers_only() {
    assert_eq!(parse_version("12").expect("ok"), 12);
    assert!(parse_version("abc").is_err());
    assert!(parse_version("").is_err());
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
fn classify_put_response_2xx_returns_version() {
    let got = classify_put_response(StatusCode::OK, r#"{"data":{"version":3}}"#).expect("ok");
    assert_eq!(got, "3");
}

#[test]
fn classify_put_response_5xx_is_unavailable() {
    let err = classify_put_response(StatusCode::SERVICE_UNAVAILABLE, "").unwrap_err();
    assert!(err.is_unavailable());
}

#[test]
fn classify_put_response_400_is_internal() {
    let err = classify_put_response(StatusCode::BAD_REQUEST, "{}").unwrap_err();
    assert!(matches!(err, CredStoreError::Internal(_)));
}

#[test]
fn classify_metadata_response_404_is_empty() {
    assert!(
        classify_metadata_response(StatusCode::NOT_FOUND, "")
            .expect("ok")
            .is_empty()
    );
}

#[test]
fn classify_destroy_response_404_and_204_are_ok() {
    classify_destroy_response(StatusCode::NO_CONTENT).expect("ok");
    classify_destroy_response(StatusCode::NOT_FOUND).expect("ok");
    assert!(
        classify_destroy_response(StatusCode::BAD_GATEWAY)
            .unwrap_err()
            .is_unavailable()
    );
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
