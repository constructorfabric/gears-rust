//! Hermetic mint -> JWKS -> verify integration test for the token-issuer gear.
//!
//! Proves the gear assembles a correct, JWKS-verifiable ES256 capability JWT
//! using *real* ES256 crypto, without Docker / a live `OpenBao` Transit:
//! a local `p256` signer stands in for the Transit signing port. Its raw `r||s`
//! signature bytes are exactly the JWS ES256 form Transit emits with
//! `marshaling_algorithm=jws`, so the assembled JWT verifies against the served
//! JWKS via `jsonwebtoken`.
//!
//! The real Transit round-trip (gRPC + `OpenBao`) is covered by the
//! `openbao-credstore` crate's signing tests; a full live-cluster Transit e2e
//! (deployed `OpenBao`) is deferred — it needs a provisioned cluster, which is
//! not reliably available in CI.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use async_trait::async_trait;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use p256::ecdsa::{Signature, SigningKey, signature::Signer};
use p256::pkcs8::{EncodePublicKey, LineEnding};
use rand_core::OsRng;
use token_issuer::config::TokenIssuerConfig;
use token_issuer::domain::DomainError;
use token_issuer::domain::metrics::TokenIssuerMetrics;
use token_issuer::domain::peer_identity::{PeerConnInfo, PeerIdentityResolver};
use token_issuer::domain::rms_registry::{AdapterRecord, RmsAdapterRegistry};
use token_issuer::domain::service::Service;
use token_issuer_sdk::{
    MintCapabilityRequest, PublicKeyVersion, SigAlg, SignatureResult, SigningClientV1,
    SigningError, SigningKeyRef,
};
use toolkit_security::SecurityContext;
use uuid::Uuid;

const EXPECTED_AUD: &str = "gts.cf.rms._.adapter.v1~acme.rms._.s3.v1";
const ISSUER_BASE_URL: &str = "https://core.test";

/// Local ES256 signer backed by a real `p256` key. Stands in for the Transit
/// signing port; produces genuine ES256 signatures in raw `r||s` (JWS) form.
struct LocalEs256Signer {
    key: SigningKey,
}

impl LocalEs256Signer {
    fn new() -> Self {
        Self {
            key: SigningKey::random(&mut OsRng),
        }
    }
}

#[async_trait]
impl SigningClientV1 for LocalEs256Signer {
    async fn sign(
        &self,
        _ctx: &SecurityContext,
        _key: &SigningKeyRef,
        signing_input: &[u8],
    ) -> Result<SignatureResult, SigningError> {
        // ECDSA/P-256 fixed-size signature == raw r||s (64 bytes), identical to
        // Transit's `marshaling_algorithm=jws` output and to the JWS ES256 form.
        let sig: Signature = self.key.sign(signing_input);
        Ok(SignatureResult {
            signature: sig.to_bytes().to_vec(),
            key_version: 1,
        })
    }

    async fn public_keys(
        &self,
        _ctx: &SecurityContext,
        _key: &SigningKeyRef,
    ) -> Result<Vec<PublicKeyVersion>, SigningError> {
        let pem = p256::PublicKey::from(self.key.verifying_key())
            .to_public_key_pem(LineEnding::LF)
            .expect("encode verifying key to PKCS#8 PEM");
        Ok(vec![PublicKeyVersion {
            version: 1,
            alg: SigAlg::Es256,
            public_key_pem: pem,
        }])
    }
}

/// Fail-closed peer resolver (no mTLS cert) — these tests exercise only the cap
/// path, so OBO collaborators are never invoked.
struct NoPeer;

#[async_trait]
impl PeerIdentityResolver for NoPeer {
    async fn resolve(&self, _peer: &PeerConnInfo) -> Result<String, DomainError> {
        Err(DomainError::PeerUnverified)
    }
}

/// Empty registry — never consulted by the cap path.
struct NoRegistry;

#[async_trait]
impl RmsAdapterRegistry for NoRegistry {
    async fn lookup(&self, _gts_id: &str) -> Result<Option<AdapterRecord>, DomainError> {
        Ok(None)
    }
    async fn gts_id_by_cert_subject(&self, _subject: &str) -> Result<Option<String>, DomainError> {
        Ok(None)
    }
}

fn test_config() -> TokenIssuerConfig {
    TokenIssuerConfig {
        issuer_base_url: ISSUER_BASE_URL.to_owned(),
        ..Default::default()
    }
}

fn caller_ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(0x1111))
        .subject_tenant_id(Uuid::from_u128(0x2222))
        .subject_type("user")
        .token_scopes(vec!["quotas:read".to_owned()])
        .build()
        .expect("build caller security context")
}

fn mint_request(context_tenant: Uuid) -> MintCapabilityRequest {
    MintCapabilityRequest {
        context_tenant,
        context_project_id: None,
        audience: EXPECTED_AUD.to_owned(),
        operation: None,
        resource_type: None,
    }
}

/// Builds a Service over the local ES256 signer with a clock anchored at the
/// real "now". `jsonwebtoken` validates `exp` against the real wall clock, so
/// the minted token (`exp = now + cap_ttl_secs`) must sit in the future.
fn service(now: i64) -> Service {
    Service::new(
        Arc::new(LocalEs256Signer::new()),
        Arc::new(NoPeer),
        Arc::new(NoRegistry),
        &test_config(),
        Arc::new(TokenIssuerMetrics::from_global()),
    )
    .expect("build service")
    .with_clock(std::sync::Arc::new(move || now))
}

#[tokio::test]
async fn mint_then_verify_against_served_jwks() {
    let now = chrono::Utc::now().timestamp();
    let svc = service(now);
    svc.warm_jwks().await.expect("warm JWKS");

    let context_tenant = Uuid::from_u128(0x42);
    let jwt = svc
        .mint_capability(&caller_ctx(), mint_request(context_tenant))
        .await
        .expect("mint capability");

    // --- Pull the single served JWK and build a verification key from x/y. ---
    let jwks = svc.cap_jwks().await.expect("cap JWKS present after warm");
    let keys = jwks["keys"].as_array().expect("keys array");
    assert_eq!(keys.len(), 1, "exactly one capability key is served");
    let jwk = &keys[0];
    assert_eq!(jwk["kty"], "EC");
    assert_eq!(jwk["crv"], "P-256");
    assert_eq!(jwk["alg"], "ES256");
    assert_eq!(jwk["use"], "sig");
    assert_eq!(jwk["kid"], "cap-token-sign-v1");
    let x = jwk["x"].as_str().expect("jwk x");
    let y = jwk["y"].as_str().expect("jwk y");
    // jsonwebtoken's from_ec_components base64url-no-pad-decodes x/y, matching
    // the JWKS builder's encoding.
    let decoding_key = DecodingKey::from_ec_components(x, y).expect("decoding key from x/y");

    // --- Verify the JWT: signature + iss + aud + exp. ---
    let mut validation = Validation::new(Algorithm::ES256);
    validation.set_issuer(&[format!("{ISSUER_BASE_URL}/issuers/cap")]);
    validation.set_audience(&[EXPECTED_AUD]);
    // validate_exp stays on (default); leeway covers the anchored "now".

    let data = decode::<serde_json::Value>(&jwt, &decoding_key, &validation)
        .expect("JWT verifies against served JWKS");

    // Header assertions.
    assert_eq!(data.header.alg, Algorithm::ES256);
    assert_eq!(data.header.typ.as_deref(), Some("cap+jwt"));
    assert_eq!(data.header.kid.as_deref(), Some("cap-token-sign-v1"));

    // Claim assertions.
    let claims = &data.claims;
    assert_eq!(claims["iss"], format!("{ISSUER_BASE_URL}/issuers/cap"));
    assert_eq!(claims["aud"], EXPECTED_AUD);
    assert_eq!(claims["context_tenant"], context_tenant.to_string());
    assert_eq!(claims["scopes"], "quotas:read");
    assert_eq!(claims["sub"], Uuid::from_u128(0x1111).to_string());
    assert_eq!(
        claims["subject_tenant"],
        Uuid::from_u128(0x2222).to_string()
    );
    assert_eq!(claims["user_type"], "user");
}

#[tokio::test]
async fn tampered_signature_fails_verification() {
    let now = chrono::Utc::now().timestamp();
    let svc = service(now);
    svc.warm_jwks().await.expect("warm JWKS");

    let jwt = svc
        .mint_capability(&caller_ctx(), mint_request(Uuid::from_u128(0x42)))
        .await
        .expect("mint capability");

    let jwks = svc.cap_jwks().await.expect("cap JWKS present after warm");
    let jwk = &jwks["keys"][0];
    let decoding_key = DecodingKey::from_ec_components(
        jwk["x"].as_str().expect("jwk x"),
        jwk["y"].as_str().expect("jwk y"),
    )
    .expect("decoding key");

    // Flip one character in the signature segment (third part).
    let mut parts: Vec<String> = jwt.split('.').map(str::to_owned).collect();
    assert_eq!(parts.len(), 3, "JWT has three segments");
    let sig = &parts[2];
    let last = sig.chars().next_back().expect("non-empty signature");
    // Pick a different base64url char so the signature really changes.
    let replacement = if last == 'A' { 'B' } else { 'A' };
    let tampered_sig: String = {
        let mut s: Vec<char> = sig.chars().collect();
        let n = s.len();
        s[n - 1] = replacement;
        s.into_iter().collect()
    };
    parts[2] = tampered_sig;
    let tampered = parts.join(".");

    // Sanity: header still decodes (we only touched the signature).
    decode_header(&tampered).expect("header still decodes");

    let mut validation = Validation::new(Algorithm::ES256);
    validation.set_issuer(&[format!("{ISSUER_BASE_URL}/issuers/cap")]);
    validation.set_audience(&[EXPECTED_AUD]);

    let result = decode::<serde_json::Value>(&tampered, &decoding_key, &validation);
    assert!(
        result.is_err(),
        "verification must fail for a tampered signature"
    );
}
