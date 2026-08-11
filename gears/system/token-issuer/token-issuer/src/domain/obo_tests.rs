use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use token_issuer_sdk::{PublicKeyVersion, SigAlg, SignatureResult, SigningError};

use super::*;

struct MockSigner {
    key_version: u32,
}

#[async_trait]
impl SigningClientV1 for MockSigner {
    async fn sign(
        &self,
        _ctx: &SecurityContext,
        _key: &SigningKeyRef,
        _signing_input: &[u8],
    ) -> Result<SignatureResult, SigningError> {
        Ok(SignatureResult {
            signature: vec![0xCD; 64],
            key_version: self.key_version,
        })
    }

    async fn public_keys(
        &self,
        _ctx: &SecurityContext,
        _key: &SigningKeyRef,
    ) -> Result<Vec<PublicKeyVersion>, SigningError> {
        Ok(vec![PublicKeyVersion {
            version: self.key_version,
            alg: SigAlg::Es256,
            public_key_pem: String::new(),
        }])
    }
}

const OBO_ISS: &str = "https://core.example.com/issuers/obo";
const ADAPTER: &str = "gts.cf.rms._.adapter.v1~acme.rms._.s3.v1";

fn cap_claims() -> CapabilityClaims {
    CapabilityClaims {
        iss: "https://core.example.com/issuers/cap".to_owned(),
        aud: ADAPTER.to_owned(),
        sub: Uuid::from_u128(0x1111),
        subject_tenant: Uuid::from_u128(0x2222),
        user_type: Some("user".to_owned()),
        context_tenant: Uuid::from_u128(0x42),
        context_project_id: None,
        scopes: "quotas:read quotas:write".to_owned(),
        jti: Uuid::from_u128(0xABCD),
        iat: 1_000,
        exp: 1_300,
        act: None,
        operation: None,
        resource_type: None,
    }
}

#[test]
fn build_obo_claims_copies_and_downscopes() {
    let cap = cap_claims();
    let claims = build_obo_claims(
        &cap,
        &["quotas:read".to_owned()],
        ADAPTER,
        OBO_ISS,
        "public-api",
        60,
        1_100,
    )
    .unwrap();
    assert_eq!(claims.iss, OBO_ISS);
    assert_eq!(claims.aud, "public-api");
    assert_eq!(claims.sub, cap.sub); // copied verbatim
    assert_eq!(claims.user_type.as_deref(), Some("user")); // copied verbatim
    assert_eq!(claims.tenant_id, cap.subject_tenant); // subject_tenant -> tenant_id
    assert_eq!(claims.act, ADAPTER);
    assert_eq!(claims.scope, "quotas:read"); // down-scoped, space-joined
    assert_eq!(claims.exp - claims.iat, 60); // now (1100) + ttl (60) < cap.exp (1300)
    assert_eq!(claims.iat, 1_100);
    assert_ne!(claims.jti, cap.jti); // fresh jti
}

#[test]
fn build_obo_claims_exp_is_decoupled_from_cap_exp() {
    let cap = cap_claims(); // cap.exp = 1300
    // exp is `now + ttl` (1290 + 60 = 1350), decoupled from cap.exp (DESIGN.md
    // § 3.1, § 2.1): a re-mint near cap expiry still yields a full-TTL OBO. The OBO
    // carries identity, not authz — the PDP re-checks live permissions.
    let claims = build_obo_claims(
        &cap,
        &["quotas:read".to_owned()],
        ADAPTER,
        OBO_ISS,
        "public-api",
        60,
        1_290,
    )
    .unwrap();
    assert_eq!(
        claims.exp, 1_350,
        "OBO exp must be now + ttl, not clamped to the cap's exp"
    );
}

#[test]
fn obo_scope_is_space_joined_and_never_wildcard() {
    let cap = cap_claims();
    let claims = build_obo_claims(
        &cap,
        &["a:b".to_owned(), "c:d".to_owned()],
        ADAPTER,
        OBO_ISS,
        "public-api",
        60,
        1_100,
    )
    .unwrap();
    assert_eq!(claims.scope, "a:b c:d");
    assert!(!claims.scope.split(' ').any(|s| s == "*"));
}

#[tokio::test]
async fn sign_obo_produces_es256_obo_jwt_with_versioned_kid() {
    let signer = MockSigner { key_version: 3 };
    let ctx = SecurityContext::anonymous();
    let key = SigningKeyRef::new("obo-token-sign").unwrap();
    let cap = cap_claims();
    let claims = build_obo_claims(
        &cap,
        &["quotas:read".to_owned()],
        ADAPTER,
        OBO_ISS,
        "public-api",
        60,
        1_100,
    )
    .unwrap();

    let jwt = sign_obo(&signer, &ctx, &key, &claims).await.unwrap();
    let parts: Vec<&str> = jwt.split('.').collect();
    assert_eq!(parts.len(), 3);

    let hdr: serde_json::Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
    assert_eq!(hdr["alg"], "ES256");
    assert_eq!(hdr["typ"], "obo+jwt");
    assert_eq!(hdr["kid"], "obo-token-sign-v3");

    let payload: OboClaims =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
    assert_eq!(payload.iss, OBO_ISS);
    assert_eq!(payload.aud, "public-api");
    assert_eq!(payload.scope, "quotas:read");
    assert_eq!(payload.tenant_id, cap.subject_tenant);
    assert_eq!(payload.act, ADAPTER);
}
