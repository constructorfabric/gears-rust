// Created: 2026-09-07 by Constructor Tech
//! The handle: opaque, self-describing to this service only, and strict.

use settings_service_sdk::SecretHandle;

use super::{HandleClaims, decode_handle, issue_handle};
use crate::domain::error::DomainError;
use crate::field;

const KEY: &str = "gts.cf.core.settings.setting_type.v1~cf.demo.security.api_token.v1~";

#[test]
fn a_handle_round_trips_and_carries_only_the_key_and_the_scope() {
    let handle = issue_handle(KEY, "/tenants/00000000-0000-0000-0000-000000000002");
    assert!(handle.as_token().starts_with("sh1."));
    assert_eq!(
        decode_handle(&handle).expect("decodes"),
        HandleClaims {
            key: KEY.to_owned(),
            scope: "/tenants/00000000-0000-0000-0000-000000000002".to_owned(),
        }
    );
    // Not a store reference and not plaintext: nothing but the two fields.
    let json = String::from_utf8(
        base64::Engine::decode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            &handle.as_token()["sh1.".len()..],
        )
        .expect("base64"),
    )
    .expect("utf8");
    let value: serde_json::Value = serde_json::from_str(&json).expect("json");
    let fields: Vec<&String> = value.as_object().expect("object").keys().collect();
    assert_eq!(fields, vec!["key", "scope"]);
}

#[test]
fn a_malformed_handle_is_an_invalid_argument_that_does_not_echo_the_token() {
    for raw in [
        "",
        "x",
        "sh1.",
        "sh1.!!!",
        "sh2.e30",
        "sh1.e30",
        &format!(
            "sh1.{}",
            base64::Engine::encode(
                &base64::engine::general_purpose::URL_SAFE_NO_PAD,
                br#"{"key":"","scope":"/"}"#
            )
        ),
    ] {
        let err = decode_handle(&SecretHandle::new(raw)).expect_err("refused");
        match err {
            DomainError::Validation {
                field: f,
                code,
                message,
            } => {
                assert_eq!(f, "handle");
                assert_eq!(code, field::SECRET_HANDLE_MALFORMED);
                assert!(!message.contains(raw) || raw.is_empty(), "{message}");
            }
            other => panic!("{other:?}"),
        }
    }
}
