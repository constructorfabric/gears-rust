use time::OffsetDateTime;
use uuid::Uuid;

use super::*;

fn sample_claims(op: Op, exp: i64) -> Claims {
    Claims {
        op,
        file_id: Uuid::now_v7(),
        version_id: Uuid::now_v7(),
        backend_id: "local".to_owned(),
        backend_path: "/f/v".to_owned(),
        exp,
        upload: UploadConstraints {
            max_size: Some(1024),
            exact_size: None,
            expected_hash: Some("SHA-256:deadbeef".to_owned()),
        },
        multipart: MultipartClaims::default(),
        request_id: "test-request-id".to_owned(),
        content_type: "image/png".to_owned(),
        etag: "\"abc123\"".to_owned(),
        bind_on_finalize: false,
    }
}

fn now() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap()
}

#[test]
fn issued_token_verifies_and_round_trips_claims() {
    let issuer = Issuer::generate(3600).unwrap();
    let verifier = issuer.verifier();
    let claims = sample_claims(Op::Get, now().unix_timestamp() + 60);

    let token = issuer.issue(claims.clone(), now()).unwrap();
    let got = verifier.verify(&token, now()).unwrap();
    assert_eq!(got, claims);
}

#[test]
fn lifetime_is_clamped_to_max_ttl() {
    let issuer = Issuer::generate(100).unwrap();
    let verifier = issuer.verifier();
    // Request a far-future expiry; issuer must clamp to now + 100.
    let claims = sample_claims(Op::Put, now().unix_timestamp() + 10_000);
    let token = issuer.issue(claims, now()).unwrap();
    let got = verifier.verify(&token, now()).unwrap();
    assert_eq!(got.exp, now().unix_timestamp() + 100);
}

#[test]
fn expiry_is_exclusive_at_the_boundary_with_and_without_grace() {
    let issuer = Issuer::generate(3600).unwrap();
    let verifier = issuer.verifier();
    let exp = now().unix_timestamp() + 60;
    let token = issuer.issue(sample_claims(Op::Put, exp), now()).unwrap();

    // (offset from exp, grace seconds, accepted). Grace only loosens the `exp` deadline.
    for (offset, grace_secs, accepted) in [
        (-1, 0, true),
        (0, 0, false),
        (30, 0, false),
        (30, 60, true),
        (59, 60, true),
        (60, 60, false),
        (90, 60, false),
    ] {
        let at = OffsetDateTime::from_unix_timestamp(exp + offset).unwrap();
        let result = verifier.verify_with_grace(&token, at, time::Duration::seconds(grace_secs));
        let case = format!("at exp{offset:+} with {grace_secs}s grace");
        match result {
            Ok(_) => assert!(accepted, "{case}: must be rejected"),
            Err(DomainError::TokenInvalid { reason }) => {
                assert!(!accepted, "{case}: must be accepted");
                assert_eq!(reason, "token expired", "{case}");
            }
            Err(other) => panic!("{case}: unexpected error {other:?}"),
        }
    }
}

#[test]
fn verify_rejects_forged_and_malformed_tokens() {
    let issuer = Issuer::generate(3600).unwrap();
    let token = issuer
        .issue(sample_claims(Op::Put, now().unix_timestamp() + 60), now())
        .unwrap();
    let (payload, sig) = token.split_once('.').unwrap();
    let mut p = payload.to_owned();
    let last = p.pop().unwrap();
    p.push(if last == 'A' { 'B' } else { 'A' });
    let tampered = format!("{p}.{sig}");
    let other_key_token = Issuer::generate(3600)
        .unwrap()
        .issue(sample_claims(Op::Put, now().unix_timestamp() + 60), now())
        .unwrap();

    // (case, token, grace, expected reason fragment). Grace never weakens the signature check.
    let cases = [
        ("tampered payload", &tampered, 0, "signature"),
        ("huge grace, tampered payload", &tampered, 3600, "signature"),
        ("signed by another key", &other_key_token, 0, "signature"),
        (
            "not a token",
            &"not-a-token".to_owned(),
            0,
            "malformed token",
        ),
    ];
    for (case, token, grace_secs, expected) in cases {
        let err = issuer
            .verifier()
            .verify_with_grace(token, now(), time::Duration::seconds(grace_secs))
            .expect_err(case);
        assert!(
            matches!(&err, DomainError::TokenInvalid { reason } if reason.contains(expected)),
            "{case}: expected TokenInvalid containing {expected:?}, got {err:?}"
        );
    }
}

#[test]
fn from_public_key_requires_exactly_32_bytes() {
    for len in [31, 33] {
        let err = Verifier::from_public_key(vec![0u8; len])
            .err()
            .expect("must be rejected");
        assert!(
            matches!(err, DomainError::TokenInvalid { .. }),
            "{len}: {err:?}"
        );
    }
    assert!(Verifier::from_public_key(vec![0u8; 32]).is_ok());
}

#[test]
fn claims_without_content_type_and_etag_deserialize_with_empty_defaults() {
    // A payload without `content_type`/`etag` keys (a pre-existing token) must deserialize
    // with both empty.
    let json = serde_json::json!({
        "op": "get",
        "file_id": Uuid::now_v7(),
        "version_id": Uuid::now_v7(),
        "backend_id": "local",
        "backend_path": "/f/v",
        "exp": now().unix_timestamp() + 60,
    });
    let claims: Claims = serde_json::from_value(json).expect("deserialize old-shape claims");
    assert_eq!(claims.content_type, "");
    assert_eq!(claims.etag, "");
}
