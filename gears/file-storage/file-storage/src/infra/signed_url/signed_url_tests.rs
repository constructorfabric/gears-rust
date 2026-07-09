use std::sync::Arc;

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
        upload: UploadConstraints::default(),
        multipart: MultipartClaims::default(),
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
fn expired_token_is_rejected() {
    let issuer = Issuer::generate(3600).unwrap();
    let verifier = issuer.verifier();
    let claims = sample_claims(Op::Get, now().unix_timestamp() + 10);
    let token = issuer.issue(claims, now()).unwrap();

    let later = now() + time::Duration::seconds(20);
    let err = verifier.verify(&token, later).unwrap_err();
    assert!(
        matches!(err, DomainError::TokenInvalid { .. }),
        "got {err:?}"
    );
}

#[test]
fn expiry_is_exclusive_at_the_boundary() {
    // A token must stop being usable exactly at `exp`, not one second later.
    let issuer = Issuer::generate(3600).unwrap();
    let verifier = issuer.verifier();
    let exp = now().unix_timestamp() + 60;
    let token = issuer.issue(sample_claims(Op::Get, exp), now()).unwrap();

    let at_exp = OffsetDateTime::from_unix_timestamp(exp).unwrap();
    assert!(
        verifier.verify(&token, at_exp).is_err(),
        "token must be rejected at now == exp"
    );
    let just_before = OffsetDateTime::from_unix_timestamp(exp - 1).unwrap();
    assert!(
        verifier.verify(&token, just_before).is_ok(),
        "token must still be valid one second before exp"
    );
}

#[test]
fn rejects_malformed_public_key() {
    assert!(Verifier::from_public_key(vec![0u8; 31]).is_err());
    assert!(Verifier::from_public_key(vec![0u8; 33]).is_err());
    // A 32-byte key (the Ed25519 public-key length) is accepted.
    assert!(Verifier::from_public_key(vec![0u8; 32]).is_ok());
}

#[test]
fn tampered_payload_fails_verification() {
    let issuer = Issuer::generate(3600).unwrap();
    let verifier = issuer.verifier();
    let token = issuer
        .issue(sample_claims(Op::Get, now().unix_timestamp() + 60), now())
        .unwrap();

    // Flip a character in the payload segment.
    let (payload, sig) = token.split_once('.').unwrap();
    let mut p = payload.to_owned();
    let last = p.pop().unwrap();
    p.push(if last == 'A' { 'B' } else { 'A' });
    let tampered = format!("{p}.{sig}");

    assert!(verifier.verify(&tampered, now()).is_err());
}

#[test]
fn token_from_other_key_is_rejected() {
    let issuer_a = Issuer::generate(3600).unwrap();
    let issuer_b = Issuer::generate(3600).unwrap();
    let token = issuer_a
        .issue(sample_claims(Op::Get, now().unix_timestamp() + 60), now())
        .unwrap();
    // Verifier for a different keypair must reject.
    assert!(issuer_b.verifier().verify(&token, now()).is_err());
}

#[test]
fn malformed_token_is_rejected() {
    let verifier = Issuer::generate(3600).unwrap().verifier();
    assert!(verifier.verify("not-a-token", now()).is_err());
}

#[test]
fn issuer_over_explicit_provider_round_trips() {
    // The codec calls the SignatureProvider abstraction (ADR-0004 FIPS posture),
    // so an issuer built over an explicitly-supplied provider behaves identically
    // — this is the seam a FIPS-validated provider plugs into.
    let provider = Arc::new(Ed25519Provider::generate().unwrap());
    let issuer = Issuer::with_provider(provider, 3600);
    let claims = sample_claims(Op::Get, now().unix_timestamp() + 60);

    let token = issuer.issue(claims.clone(), now()).unwrap();
    assert_eq!(issuer.verifier().verify(&token, now()).unwrap(), claims);
}

#[test]
fn upload_constraints_round_trip() {
    let issuer = Issuer::generate(3600).unwrap();
    let mut claims = sample_claims(Op::Put, now().unix_timestamp() + 60);
    claims.upload = UploadConstraints {
        max_size: Some(1024),
        exact_size: None,
        expected_hash: Some("SHA-256:deadbeef".to_owned()),
    };
    let token = issuer.issue(claims.clone(), now()).unwrap();
    let got = issuer.verifier().verify(&token, now()).unwrap();
    assert_eq!(got.upload, claims.upload);
}
