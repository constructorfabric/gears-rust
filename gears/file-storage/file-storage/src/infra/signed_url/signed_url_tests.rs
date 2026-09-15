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
        request_id: "test-request-id".to_owned(),
        content_type: String::new(),
        etag: String::new(),
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

// ── Multi-key `Verifier` (rotation without outage / without `kid`) ──────────

#[test]
fn from_public_key_still_works_as_a_single_key_verifier() {
    // `from_public_key` must behave exactly as before the multi-key
    // refactor: a thin single-element wrapper over `from_public_keys`.
    let issuer = Issuer::generate(3600).unwrap();
    let verifier = Verifier::from_public_key(issuer.public_key()).unwrap();
    let claims = sample_claims(Op::Get, now().unix_timestamp() + 60);
    let token = issuer.issue(claims.clone(), now()).unwrap();
    assert_eq!(verifier.verify(&token, now()).unwrap(), claims);
}

#[test]
fn from_public_keys_accepts_a_token_signed_by_any_configured_key() {
    let provider_a = Arc::new(Ed25519Provider::generate().unwrap());
    let provider_b = Arc::new(Ed25519Provider::generate().unwrap());
    let issuer_a =
        Issuer::with_provider(Arc::clone(&provider_a) as Arc<dyn SignatureProvider>, 3600);
    let claims = sample_claims(Op::Get, now().unix_timestamp() + 60);
    let token = issuer_a.issue(claims.clone(), now()).unwrap();

    // Primary (index 0) is B's key, previous (index 1) is A's -- the token
    // was signed by A, so it must still verify via the second entry. This
    // is the rotation-without-outage property: a sidecar configured with
    // [new, old] keeps accepting tokens signed by the old (still-primary
    // on the control plane) key.
    let verifier =
        Verifier::from_public_keys(vec![provider_b.public_key(), provider_a.public_key()]).unwrap();
    assert_eq!(verifier.verify(&token, now()).unwrap(), claims);
}

#[test]
fn from_public_keys_rejects_a_token_from_an_unlisted_key_with_the_same_error_as_a_single_wrong_key()
{
    let provider_a = Arc::new(Ed25519Provider::generate().unwrap());
    let provider_b = Arc::new(Ed25519Provider::generate().unwrap());
    let provider_c = Arc::new(Ed25519Provider::generate().unwrap());
    let issuer_c =
        Issuer::with_provider(Arc::clone(&provider_c) as Arc<dyn SignatureProvider>, 3600);
    let token = issuer_c
        .issue(sample_claims(Op::Get, now().unix_timestamp() + 60), now())
        .unwrap();

    let multi_key_err =
        Verifier::from_public_keys(vec![provider_b.public_key(), provider_a.public_key()])
            .unwrap()
            .verify(&token, now())
            .unwrap_err();
    let single_key_err = Verifier::from_public_key(provider_b.public_key())
        .unwrap()
        .verify(&token, now())
        .unwrap_err();

    // A caller must not be able to tell from the error how many keys were
    // configured or tried -- same message either way.
    assert_eq!(multi_key_err.to_string(), single_key_err.to_string());
}

#[test]
fn from_public_keys_rejects_an_empty_list() {
    assert!(Verifier::from_public_keys(vec![]).is_err());
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

// ── P2 1.11: `content_type` / `etag` claims (GET download tokens) ───────────

#[test]
fn content_type_and_etag_round_trip() {
    let issuer = Issuer::generate(3600).unwrap();
    let mut claims = sample_claims(Op::Get, now().unix_timestamp() + 60);
    claims.content_type = "image/png".to_owned();
    claims.etag = "\"abc123\"".to_owned();

    let token = issuer.issue(claims.clone(), now()).unwrap();
    let got = issuer.verifier().verify(&token, now()).unwrap();
    assert_eq!(got.content_type, "image/png");
    assert_eq!(got.etag, "\"abc123\"");
    assert_eq!(got, claims);
}

#[test]
fn claims_without_content_type_and_etag_deserialize_with_empty_defaults() {
    // Simulates verifying a token minted before these fields existed: a
    // JSON payload with no `content_type`/`etag` keys at all must still
    // deserialize, defaulting both to the empty string (version-skew
    // tolerance, same pattern as `request_id`/`backend_handle`).
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
