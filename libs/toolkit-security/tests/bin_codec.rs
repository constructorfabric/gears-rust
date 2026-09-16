#![allow(clippy::unwrap_used, clippy::expect_used)]

use toolkit_security::{
    SECCTX_BIN_VERSION, SecCtxDecodeError, SecurityContext, decode_bin, encode_bin,
};
use uuid::Uuid;

#[test]
#[allow(clippy::unreadable_literal)] // UUID hex patterns are intentionally repeating
fn round_trips_security_ctx_binary_payload() {
    let subject_id = Uuid::from_u128(0xdeadbeefdeadbeefdeadbeefdeadbeef);
    let subject_tenant_id = Uuid::from_u128(0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb);

    let ctx = SecurityContext::builder()
        .subject_id(subject_id)
        .subject_tenant_id(subject_tenant_id)
        .token_scopes(vec!["admin".to_owned(), "read:events".to_owned()])
        .build()
        .unwrap();

    let encoded = encode_bin(&ctx).expect("security context encodes");
    let decoded = decode_bin(&encoded).expect("security context decodes");

    // Validate core fields round-trip
    assert_eq!(decoded.subject_id(), ctx.subject_id());
    assert_eq!(decoded.subject_tenant_id(), ctx.subject_tenant_id());
    assert_eq!(decoded.token_scopes(), ctx.token_scopes());
    // bearer_token is #[serde(skip)] so not included in binary encoding
    assert!(decoded.bearer_token().is_none());
}

#[test]
#[allow(clippy::unreadable_literal)] // UUID hex patterns are intentionally repeating
fn decode_rejects_unknown_version() {
    let ctx = SecurityContext::anonymous();

    let mut encoded = encode_bin(&ctx).expect("encodes context");
    encoded[0] = SECCTX_BIN_VERSION.wrapping_add(1);

    // Assert on the variant and the version it reports, not on the `Display`
    // text: rewording the `#[error(...)]` attribute must not fail this test, and
    // returning a different variant with a similar message must not pass it.
    let err = decode_bin(&encoded).expect_err("version mismatch should error");
    let bumped = SECCTX_BIN_VERSION.wrapping_add(1);
    assert!(
        matches!(err, SecCtxDecodeError::UnsupportedVersion(v) if v == bumped),
        "expected UnsupportedVersion({bumped}), got: {err:?}"
    );
}

#[test]
fn decode_rejects_an_empty_blob() {
    // Reachable from untrusted wire input: gRPC metadata can carry an empty
    // value, and there is not even a version byte to check.
    let err = decode_bin(&[]).expect_err("an empty blob has nothing to decode");
    assert!(
        matches!(err, SecCtxDecodeError::Empty),
        "expected Empty, got: {err:?}"
    );
}

#[test]
fn decode_rejects_a_correctly_versioned_garbage_payload() {
    // The version byte agreeing proves nothing about the payload behind it.
    let blob = [SECCTX_BIN_VERSION, 0xff, 0xff, 0xff, 0xff];
    let err = decode_bin(&blob).expect_err("garbage must not deserialize");
    assert!(
        matches!(err, SecCtxDecodeError::Postcard(_)),
        "expected a postcard error, got: {err:?}"
    );
}

#[test]
fn the_version_byte_pins_the_field_layout() {
    // postcard is positional and carries no field names, so adding, removing or
    // reordering a serialized field of `SecurityContext` silently changes this
    // byte string. If this assertion fails, that is the signal to bump
    // `SECCTX_BIN_VERSION` -- not to update the expected bytes.
    let ctx = SecurityContext::builder()
        .subject_id(Uuid::from_u128(1))
        .subject_tenant_id(Uuid::from_u128(2))
        .build()
        .unwrap();

    let encoded = encode_bin(&ctx).expect("encodes");

    let mut expected = vec![SECCTX_BIN_VERSION];
    // A `Uuid` encodes as a 16-byte sequence, length-prefixed by postcard.
    expected.push(16);
    expected.extend_from_slice(&1u128.to_be_bytes()); // subject_id
    expected.push(0); // subject_type: Option::None
    expected.push(16);
    expected.extend_from_slice(&2u128.to_be_bytes()); // subject_tenant_id
    expected.push(0); // token_scopes: empty sequence

    assert_eq!(
        encoded, expected,
        "v{SECCTX_BIN_VERSION} layout changed; bump SECCTX_BIN_VERSION rather than \
         editing this expectation, or an older decoder will misread the new bytes"
    );
}
