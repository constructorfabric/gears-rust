#![allow(clippy::unwrap_used, clippy::expect_used)]

use time::OffsetDateTime;
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

/// The v2 postcard payload for a context with subject 1, tenant 2, no
/// subject type, no scopes, and `expires_at` as given, without the version
/// byte.
fn layout_payload(expires_at: Option<&str>) -> Vec<u8> {
    // A `Uuid` encodes as a 16-byte sequence, length-prefixed by postcard.
    let mut payload = vec![16];
    payload.extend_from_slice(&1u128.to_be_bytes()); // subject_id
    payload.push(0); // subject_type: Option::None
    payload.push(16);
    payload.extend_from_slice(&2u128.to_be_bytes()); // subject_tenant_id
    payload.push(0); // token_scopes: empty sequence
    match expires_at {
        None => payload.push(0), // expires_at: Option::None
        Some(text) => {
            payload.push(1); // expires_at: Option::Some
            payload.push(u8::try_from(text.len()).unwrap()); // varint length
            payload.extend_from_slice(text.as_bytes());
        }
    }
    payload
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
    expected.extend(layout_payload(None));
    assert_eq!(
        encoded, expected,
        "v{SECCTX_BIN_VERSION} layout changed; bump SECCTX_BIN_VERSION rather than \
         editing this expectation, or an older decoder will misread the new bytes"
    );
}

#[test]
fn decode_rejects_the_v1_layout_that_had_no_expires_at() {
    // v1 ended after `token_scopes`. A v1 producer must get a clean
    // `UnsupportedVersion`, not a positional misread of its bytes.
    let mut v1 = vec![1];
    let mut payload = layout_payload(None);
    payload.pop(); // v1 had no `expires_at`
    v1.extend(payload);

    let err = decode_bin(&v1).expect_err("the v1 layout is no longer accepted");
    assert!(
        matches!(err, SecCtxDecodeError::UnsupportedVersion(1)),
        "expected UnsupportedVersion(1), got: {err:?}"
    );
    assert_ne!(
        SECCTX_BIN_VERSION, 1,
        "adding expires_at changed the layout"
    );
}

#[test]
fn decode_rejects_a_malformed_expires_at() {
    let mut blob = vec![SECCTX_BIN_VERSION];
    blob.extend(layout_payload(Some("not a timestamp")));

    let err = decode_bin(&blob).expect_err("expires_at must be RFC 3339");
    assert!(
        matches!(err, SecCtxDecodeError::Postcard(_)),
        "expected a postcard error, got: {err:?}"
    );
}

#[test]
fn decode_rejects_an_expires_at_whose_utc_instant_is_out_of_range() {
    // Parses as a valid RFC 3339 timestamp, but is year 10000 in UTC.
    let mut blob = vec![SECCTX_BIN_VERSION];
    blob.extend(layout_payload(Some("9999-12-31T23:59:59-01:00")));

    let err = decode_bin(&blob).expect_err("out-of-range expiry must be an error");
    assert!(
        matches!(err, SecCtxDecodeError::Postcard(_)),
        "expected a postcard error, got: {err:?}"
    );
}

#[test]
fn round_trips_expires_at() {
    let expires_at = OffsetDateTime::from_unix_timestamp(1_900_000_000).unwrap();
    let ctx = SecurityContext::builder()
        .subject_id(Uuid::from_u128(1))
        .subject_tenant_id(Uuid::from_u128(2))
        .expires_at(expires_at)
        .build()
        .unwrap();

    let decoded = decode_bin(&encode_bin(&ctx).unwrap()).unwrap();

    assert_eq!(decoded.expires_at(), Some(expires_at));
}

/// The builder accepts any instant with a UTC representation, but RFC 3339
/// has no year before 0000: such a context builds, and both encodings refuse
/// it with an error rather than writing an unreadable timestamp.
#[test]
fn an_expiry_before_year_zero_builds_but_does_not_encode() {
    let expires_at = time::Date::from_calendar_date(-1, time::Month::December, 31)
        .unwrap()
        .midnight()
        .assume_utc();
    let ctx = SecurityContext::builder()
        .subject_id(Uuid::from_u128(1))
        .subject_tenant_id(Uuid::from_u128(2))
        .expires_at(expires_at)
        .build()
        .expect("representable in UTC");

    assert!(encode_bin(&ctx).is_err(), "postcard");
    assert!(serde_json::to_value(&ctx).is_err(), "JSON");
}

/// UTC instants RFC 3339 can carry: years 0000 through 9999.
fn rfc3339_utc_range() -> std::ops::RangeInclusive<i128> {
    let at = |year, month, day, time| {
        time::Date::from_calendar_date(year, month, day)
            .unwrap()
            .with_time(time)
            .assume_utc()
            .unix_timestamp_nanos()
    };
    let first = at(0, time::Month::January, 1, time::Time::MIDNIGHT);
    let last_time = time::Time::from_hms_nano(23, 59, 59, 999_999_999).unwrap();
    let last = at(9999, time::Month::December, 31, last_time);
    first..=last
}

proptest::proptest! {
    #[test]
    fn expires_at_survives_json_and_postcard_in_any_offset(
        nanos in rfc3339_utc_range(),
        offset_minutes in -(23 * 60 + 59)..=(23 * 60 + 59_i32),
    ) {
        let instant = OffsetDateTime::from_unix_timestamp_nanos(nanos).unwrap();
        let offset = time::UtcOffset::from_whole_seconds(offset_minutes * 60).unwrap();
        let local = instant.checked_to_offset(offset);
        // The local rendering must itself be a four-digit RFC 3339 year.
        proptest::prop_assume!(local.is_some_and(|t| (0..=9999).contains(&t.year())));
        let ctx = SecurityContext::builder()
            .subject_id(Uuid::from_u128(1))
            .subject_tenant_id(Uuid::from_u128(2))
            .expires_at(local.unwrap())
            .build()
            .unwrap();

        let from_bin = decode_bin(&encode_bin(&ctx).unwrap()).unwrap();
        let from_json: SecurityContext =
            serde_json::from_value(serde_json::to_value(&ctx).unwrap()).unwrap();

        for decoded in [from_bin, from_json] {
            let expires_at = decoded.expires_at().unwrap();
            proptest::prop_assert_eq!(expires_at, instant);
            proptest::prop_assert_eq!(expires_at.offset(), time::UtcOffset::UTC);
        }
    }
}
