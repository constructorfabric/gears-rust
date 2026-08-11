use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use p256::ecdsa::{Signature, SigningKey, signature::Signer as _};
use p256::pkcs8::{EncodePublicKey, LineEnding};
use rand_core::OsRng;
use serde_json::json;

use async_trait::async_trait;
use secrecy::SecretString;
use token_issuer_sdk::{PublicKeyVersion, SigAlg, SignatureResult, SigningError};
use uuid::Uuid;

use super::*;
use crate::domain::jwks::jwks_document;
use crate::domain::rms_registry::AdapterRecord;

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
            signature: vec![0xAB; 64],
            key_version: self.key_version,
        })
    }

    async fn public_keys(
        &self,
        _ctx: &SecurityContext,
        _key: &SigningKeyRef,
    ) -> Result<Vec<PublicKeyVersion>, SigningError> {
        let key = SigningKey::random(&mut OsRng);
        let pem = p256::PublicKey::from(key.verifying_key())
            .to_public_key_pem(LineEnding::LF)
            .unwrap();
        Ok(vec![PublicKeyVersion {
            version: self.key_version,
            alg: SigAlg::Es256,
            public_key_pem: pem,
        }])
    }
}

/// Signer that can mint but has no publishable keys.
struct EmptyKeySigner {
    key_version: u32,
}

#[async_trait]
impl SigningClientV1 for EmptyKeySigner {
    async fn sign(
        &self,
        _ctx: &SecurityContext,
        _key: &SigningKeyRef,
        _signing_input: &[u8],
    ) -> Result<SignatureResult, SigningError> {
        Ok(SignatureResult {
            signature: vec![0xAB; 64],
            key_version: self.key_version,
        })
    }

    async fn public_keys(
        &self,
        _ctx: &SecurityContext,
        _key: &SigningKeyRef,
    ) -> Result<Vec<PublicKeyVersion>, SigningError> {
        Ok(vec![])
    }
}

/// A peer resolver that always yields a fixed GTS ID (a verified mTLS peer).
struct MockPeerResolver {
    gts_id: String,
}

#[async_trait]
impl PeerIdentityResolver for MockPeerResolver {
    async fn resolve(&self, _peer: &PeerConnInfo) -> Result<String, DomainError> {
        Ok(self.gts_id.clone())
    }
}

/// A registry returning a fixed adapter record for any GTS ID lookup.
struct MockRegistry {
    record: Option<AdapterRecord>,
}

#[async_trait]
impl RmsAdapterRegistry for MockRegistry {
    async fn lookup(&self, _gts_id: &str) -> Result<Option<AdapterRecord>, DomainError> {
        Ok(self.record.clone())
    }

    async fn gts_id_by_cert_subject(&self, _subject: &str) -> Result<Option<String>, DomainError> {
        Ok(None)
    }
}

fn test_ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(1))
        .subject_tenant_id(Uuid::from_u128(2))
        .subject_type("user")
        .token_scopes(vec!["b".to_owned(), "a".to_owned()])
        .build()
        .expect("test ctx")
}

fn sample_req() -> MintCapabilityRequest {
    MintCapabilityRequest {
        context_tenant: Uuid::from_u128(42),
        context_project_id: None,
        audience: "aud".to_owned(),
        operation: None,
        resource_type: None,
    }
}

fn config() -> TokenIssuerConfig {
    TokenIssuerConfig {
        issuer_base_url: "https://core.example.com".to_owned(),
        ..Default::default()
    }
}

fn service(version: u32) -> Service {
    service_with(version, &config(), None, None)
}

/// Builds a `Service` with overridable OBO collaborators. `peer_gts` (when set)
/// is what the peer resolver returns; `record` is what the registry yields.
fn service_with(
    version: u32,
    cfg: &TokenIssuerConfig,
    peer_gts: Option<&str>,
    record: Option<AdapterRecord>,
) -> Service {
    let peer_resolver: Arc<dyn PeerIdentityResolver> = Arc::new(MockPeerResolver {
        gts_id: peer_gts.unwrap_or("gts.unset").to_owned(),
    });
    let registry: Arc<dyn RmsAdapterRegistry> = Arc::new(MockRegistry { record });
    Service::new(
        Arc::new(MockSigner {
            key_version: version,
        }),
        peer_resolver,
        registry,
        cfg,
        Arc::new(TokenIssuerMetrics::from_global()),
    )
    .unwrap()
    .with_clock(Arc::new(|| 1_000))
}

const ADAPTER_GTS: &str = "gts.cf.rms._.adapter.v1~acme.rms._.s3.v1";
const CAP_KID: &str = "cap-token-sign-v1";

/// OBO-enabled config (issuer base + `obo.enabled = true`).
fn obo_config() -> TokenIssuerConfig {
    let mut cfg = config();
    cfg.obo.enabled = true;
    cfg
}

/// b64url-no-pad JSON encode.
fn b64(v: &serde_json::Value) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(v).unwrap())
}

/// Signs a cap-shaped JWT with a fresh P-256 key (`aud = ADAPTER_GTS`,
/// `scopes = "quotas:read quotas:write"`) and returns the JWT plus the
/// single-key cap JWKS that verifies it. `iss`/`exp` are parameterized.
fn sign_cap_jwt(iss: &str, exp: i64) -> (String, serde_json::Value) {
    let key = SigningKey::random(&mut OsRng);
    let pem = p256::PublicKey::from(key.verifying_key())
        .to_public_key_pem(LineEnding::LF)
        .unwrap();
    let jwks = jwks_document(
        "cap-token-sign",
        &[PublicKeyVersion {
            version: 1,
            alg: SigAlg::Es256,
            public_key_pem: pem,
        }],
    );
    let header = b64(&json!({ "alg": "ES256", "typ": "cap+jwt", "kid": CAP_KID }));
    let claims = json!({
        "iss": iss,
        "aud": ADAPTER_GTS,
        "sub": Uuid::from_u128(0x1111),
        "subject_tenant": Uuid::from_u128(0x2222),
        "user_type": "user",
        "context_tenant": Uuid::from_u128(0x42),
        "scopes": "quotas:read quotas:write",
        "jti": Uuid::from_u128(0x1234),
        "iat": exp - 300,
        "exp": exp,
    });
    let payload = b64(&claims);
    let signing_input = format!("{header}.{payload}");
    let sig: Signature = key.sign(signing_input.as_bytes());
    let jwt = format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(sig.to_bytes()));
    (jwt, jwks)
}

fn granted_adapter(scopes: &[&str]) -> AdapterRecord {
    AdapterRecord {
        status_active: true,
        obo_callback_enabled: true,
        obo_scope_allowlist: scopes.iter().map(|s| (*s).to_owned()).collect(),
    }
}

/// Installs `jwks` as the service's cap JWKS so `verify_cap` can resolve the
/// test key (bypasses `warm_jwks`, which would call the mock signer).
async fn install_cap_jwks(svc: &Service, jwks: serde_json::Value) {
    *svc.cap.jwks.doc.write().await = Some(jwks);
}

const CAP_ISS: &str = "https://core.example.com/issuers/cap";
const OBO_ISS: &str = "https://core.example.com/issuers/obo";

#[tokio::test]
async fn mint_capability_produces_es256_cap_jwt() {
    // Real-key signer: the A7 publishability gate fails the mint if the signed kid
    // can't be published in the JWKS, so a structure check needs a signer with keys.
    let svc = pubkey_service(Arc::new(PubKeySigner::new(2)));
    let jwt = svc
        .mint_capability(&test_ctx(), sample_req())
        .await
        .unwrap();
    let parts: Vec<&str> = jwt.split('.').collect();
    assert_eq!(parts.len(), 3);

    let hdr: serde_json::Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
    assert_eq!(hdr["alg"], "ES256");
    assert_eq!(hdr["typ"], "cap+jwt");
    assert_eq!(hdr["kid"], "cap-token-sign-v2");

    let payload: token_issuer_sdk::CapabilityClaims =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
    assert_eq!(payload.scopes, "a b");
    assert_eq!(payload.aud, "aud");
}

#[tokio::test]
async fn warm_jwks_fails_closed_on_empty_key_set() {
    // EmptyKeySigner yields no public keys → fail closed (NotReady), and the
    // cache stays unwarmed.
    let svc = Service::new(
        Arc::new(EmptyKeySigner { key_version: 1 }),
        Arc::new(MockPeerResolver {
            gts_id: "gts.unset".to_owned(),
        }),
        Arc::new(MockRegistry { record: None }),
        &config(),
        Arc::new(TokenIssuerMetrics::from_global()),
    )
    .unwrap();
    assert!(matches!(svc.cap_jwks().await, Err(DomainError::NotReady)));
    assert!(matches!(svc.warm_jwks().await, Err(DomainError::NotReady)));
    assert!(matches!(svc.cap_jwks().await, Err(DomainError::NotReady)));
}

/// Signer whose `public_keys` always errors — models a signing backend that is
/// unreachable or not yet registered at warm time.
struct UnavailableSigner;

#[async_trait]
impl SigningClientV1 for UnavailableSigner {
    async fn sign(
        &self,
        _ctx: &SecurityContext,
        _key: &SigningKeyRef,
        _signing_input: &[u8],
    ) -> Result<SignatureResult, SigningError> {
        Err(SigningError::ServiceUnavailable {
            detail: "signing backend unavailable".to_owned(),
            retry_after: None,
        })
    }

    async fn public_keys(
        &self,
        _ctx: &SecurityContext,
        _key: &SigningKeyRef,
    ) -> Result<Vec<PublicKeyVersion>, SigningError> {
        Err(SigningError::ServiceUnavailable {
            detail: "signing backend unavailable".to_owned(),
            retry_after: None,
        })
    }
}

#[tokio::test]
async fn warm_jwks_fails_closed_when_signer_errors() {
    // A signing-backend error (not just an empty key set) must fail closed with
    // NotReady so the lifecycle retry loop keeps retrying instead of caching a
    // bad JWKS or giving up permanently.
    let peer_resolver: Arc<dyn PeerIdentityResolver> = Arc::new(MockPeerResolver {
        gts_id: "gts.unset".to_owned(),
    });
    let registry: Arc<dyn RmsAdapterRegistry> = Arc::new(MockRegistry { record: None });
    let svc = Service::new(
        Arc::new(UnavailableSigner),
        peer_resolver,
        registry,
        &config(),
        Arc::new(TokenIssuerMetrics::from_global()),
    )
    .unwrap();
    assert!(matches!(svc.warm_jwks().await, Err(DomainError::NotReady)));
    assert!(matches!(svc.cap_jwks().await, Err(DomainError::NotReady)));
}

/// Signer that publishes one real P-256 public key (so `warm_jwks` builds a
/// non-empty JWKS) and signs with a fixed signature at the configured version.
struct PubKeySigner {
    pem: String,
    key_version: u32,
}

impl PubKeySigner {
    fn new(key_version: u32) -> Self {
        let key = SigningKey::random(&mut OsRng);
        let pem = p256::PublicKey::from(key.verifying_key())
            .to_public_key_pem(LineEnding::LF)
            .unwrap();
        Self { pem, key_version }
    }
}

#[async_trait]
impl SigningClientV1 for PubKeySigner {
    async fn sign(
        &self,
        _ctx: &SecurityContext,
        _key: &SigningKeyRef,
        _signing_input: &[u8],
    ) -> Result<SignatureResult, SigningError> {
        Ok(SignatureResult {
            signature: vec![0xAB; 64],
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
            public_key_pem: self.pem.clone(),
        }])
    }
}

fn pubkey_service(signer: Arc<PubKeySigner>) -> Service {
    let peer_resolver: Arc<dyn PeerIdentityResolver> = Arc::new(MockPeerResolver {
        gts_id: "gts.unset".to_owned(),
    });
    let registry: Arc<dyn RmsAdapterRegistry> = Arc::new(MockRegistry { record: None });
    Service::new(
        signer,
        peer_resolver,
        registry,
        &config(),
        Arc::new(TokenIssuerMetrics::from_global()),
    )
    .unwrap()
    .with_clock(Arc::new(|| 1_000))
}

#[tokio::test]
async fn warm_jwks_caches_nonempty_cap_document() {
    let svc = pubkey_service(Arc::new(PubKeySigner::new(1)));
    svc.warm_jwks().await.expect("warm should succeed");
    let doc = svc.cap_jwks().await.expect("cap jwks present after warm");
    assert_eq!(doc["keys"].as_array().unwrap().len(), 1);
    assert_eq!(doc["keys"][0]["kid"], "cap-token-sign-v1");
}

#[tokio::test]
async fn mint_rebuilds_cap_jwks_on_unseen_key_version() {
    // Warm at v1, then mint with a signer reporting v2 (Transit rotated): the
    // cap JWKS must gain the v2 kid so the freshly minted token verifies.
    let svc = pubkey_service(Arc::new(PubKeySigner::new(2)));
    // Seed the cache with a v1-only JWKS (as if warmed before rotation).
    let v1_signer = PubKeySigner::new(1);
    let v1_doc = jwks_document(
        "cap-token-sign",
        &[PublicKeyVersion {
            version: 1,
            alg: SigAlg::Es256,
            public_key_pem: v1_signer.pem.clone(),
        }],
    );
    *svc.cap.jwks.doc.write().await = Some(v1_doc);

    svc.mint_capability(&test_ctx(), sample_req())
        .await
        .unwrap();

    let doc = svc.cap_jwks().await.unwrap();
    let kids: Vec<&str> = doc["keys"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|k| k["kid"].as_str())
        .collect();
    assert!(
        kids.contains(&"cap-token-sign-v2"),
        "rotated kid present: {kids:?}"
    );
}

#[tokio::test]
async fn discovery_documents_point_at_jwks() {
    let svc = service(1);
    let cap = svc.cap_discovery();
    assert_eq!(cap["issuer"], "https://core.example.com/issuers/cap");
    assert_eq!(
        cap["jwks_uri"],
        "https://core.example.com/issuers/cap/jwks.json"
    );
    assert!(!svc.obo_enabled());
}

// ─── remint_obo ──────────────────────────────────────────────────────────────

/// Real wall-clock seconds. `verify_cap` validates `exp` against the real clock
/// (its `now` arg is ignored), so cap-JWT expiries must be real-time relative.
fn real_now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// A `PeerConnInfo` with a (dummy) cert subject — the mock resolver ignores it.
fn peer() -> PeerConnInfo {
    PeerConnInfo {
        client_cert_subject: Some("CN=adapter-s3".to_owned()),
    }
}

/// Builds an OBO-enabled service whose peer resolves to `ADAPTER_GTS` and whose
/// registry returns `record`, with the test cap JWKS already installed.
async fn obo_service(record: Option<AdapterRecord>, jwks: serde_json::Value) -> Service {
    let svc = service_with(3, &obo_config(), Some(ADAPTER_GTS), record);
    install_cap_jwks(&svc, jwks).await;
    svc
}

#[tokio::test]
async fn remint_happy_path_mints_downscoped_obo() {
    let (cap_jwt, jwks) = sign_cap_jwt(CAP_ISS, real_now() + 300);
    let svc = obo_service(Some(granted_adapter(&["quotas:read"])), jwks).await;

    let obo = svc.remint_obo(&peer(), &cap_jwt, None).await.unwrap();
    let parts: Vec<&str> = obo.split('.').collect();
    assert_eq!(parts.len(), 3);

    let hdr: serde_json::Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
    assert_eq!(hdr["alg"], "ES256");
    assert_eq!(hdr["typ"], "obo+jwt");
    assert_eq!(hdr["kid"], "obo-token-sign-v3");

    let claims: crate::domain::obo::OboClaims =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
    assert_eq!(claims.iss, OBO_ISS);
    assert_eq!(claims.aud, "public-api");
    assert_eq!(claims.act, ADAPTER_GTS);
    // allowlist {quotas:read} ∩ cap {quotas:read, quotas:write} = {quotas:read}
    assert_eq!(claims.scope, "quotas:read");
    let obo_jwks = svc.obo_jwks().await.unwrap();
    assert_eq!(obo_jwks["keys"][0]["kid"], "obo-token-sign-v3");
}

#[tokio::test]
async fn remint_refuses_unpublishable_kid_before_caching() {
    let (cap_jwt, cap_jwks) = sign_cap_jwt(CAP_ISS, real_now() + 300);
    let cfg = obo_config();
    let svc = Service::new(
        Arc::new(EmptyKeySigner { key_version: 3 }),
        Arc::new(MockPeerResolver {
            gts_id: ADAPTER_GTS.to_owned(),
        }),
        Arc::new(MockRegistry {
            record: Some(granted_adapter(&["quotas:read"])),
        }),
        &cfg,
        Arc::new(TokenIssuerMetrics::from_global()),
    )
    .unwrap()
    .with_clock(Arc::new(|| 1_000));
    install_cap_jwks(&svc, cap_jwks).await;

    for _ in 0..2 {
        assert!(matches!(
            svc.remint_obo(&peer(), &cap_jwt, None).await,
            Err(DomainError::NotReady)
        ));
    }
    assert!(matches!(svc.obo_jwks().await, Err(DomainError::NotReady)));
}

#[tokio::test]
async fn remint_disabled_is_obo_disabled() {
    // obo.enabled = false (default config).
    let (cap_jwt, jwks) = sign_cap_jwt(CAP_ISS, real_now() + 300);
    let svc = service_with(
        3,
        &config(),
        Some(ADAPTER_GTS),
        Some(granted_adapter(&["quotas:read"])),
    );
    install_cap_jwks(&svc, jwks).await;
    assert!(matches!(
        svc.remint_obo(&peer(), &cap_jwt, None).await,
        Err(DomainError::OboDisabled)
    ));
}

#[tokio::test]
async fn remint_rejects_peer_mismatch() {
    let (cap_jwt, jwks) = sign_cap_jwt(CAP_ISS, real_now() + 300);
    // Peer resolves to a different GTS than the cap's aud (ADAPTER_GTS).
    let svc = service_with(
        3,
        &obo_config(),
        Some("gts.cf.rms._.adapter.v1~other"),
        Some(granted_adapter(&["quotas:read"])),
    );
    install_cap_jwks(&svc, jwks).await;
    assert!(matches!(
        svc.remint_obo(&peer(), &cap_jwt, None).await,
        Err(DomainError::PeerMismatch)
    ));
}

#[tokio::test]
async fn remint_rejects_unknown_peer() {
    let (cap_jwt, jwks) = sign_cap_jwt(CAP_ISS, real_now() + 300);
    // Registry has no record for the (matching) peer GTS.
    let svc = obo_service(None, jwks).await;
    assert!(matches!(
        svc.remint_obo(&peer(), &cap_jwt, None).await,
        Err(DomainError::PeerUnknown)
    ));
}

#[tokio::test]
async fn remint_rejects_inactive_adapter() {
    let (cap_jwt, jwks) = sign_cap_jwt(CAP_ISS, real_now() + 300);
    let mut rec = granted_adapter(&["quotas:read"]);
    rec.status_active = false;
    let svc = obo_service(Some(rec), jwks).await;
    assert!(matches!(
        svc.remint_obo(&peer(), &cap_jwt, None).await,
        Err(DomainError::AdapterInactive)
    ));
}

#[tokio::test]
async fn remint_rejects_obo_not_enabled_on_adapter() {
    let (cap_jwt, jwks) = sign_cap_jwt(CAP_ISS, real_now() + 300);
    let mut rec = granted_adapter(&["quotas:read"]);
    rec.obo_callback_enabled = false;
    let svc = obo_service(Some(rec), jwks).await;
    assert!(matches!(
        svc.remint_obo(&peer(), &cap_jwt, None).await,
        Err(DomainError::OboNotGranted)
    ));
}

#[tokio::test]
async fn remint_rejects_empty_intersection() {
    let (cap_jwt, jwks) = sign_cap_jwt(CAP_ISS, real_now() + 300);
    // Allowlist disjoint from the cap scopes → empty intersection → 403.
    let svc = obo_service(Some(granted_adapter(&["billing:read"])), jwks).await;
    assert!(matches!(
        svc.remint_obo(&peer(), &cap_jwt, None).await,
        Err(DomainError::OboNotGranted)
    ));
}

#[tokio::test]
async fn remint_rejects_empty_requested_scope_set() {
    let (cap_jwt, jwks) = sign_cap_jwt(CAP_ISS, real_now() + 300);
    let svc = obo_service(Some(granted_adapter(&["quotas:read"])), jwks).await;
    // requested = Some([]) → down-scope yields empty → never mint an empty OBO.
    assert!(matches!(
        svc.remint_obo(&peer(), &cap_jwt, Some(vec![])).await,
        Err(DomainError::OboNotGranted)
    ));
}

#[tokio::test]
async fn remint_rejects_requested_exceeding_grant() {
    let (cap_jwt, jwks) = sign_cap_jwt(CAP_ISS, real_now() + 300);
    let svc = obo_service(Some(granted_adapter(&["quotas:read"])), jwks).await;
    // Requesting a scope outside the grant → NotSubset → 403.
    assert!(matches!(
        svc.remint_obo(&peer(), &cap_jwt, Some(vec!["billing:write".to_owned()]))
            .await,
        Err(DomainError::OboNotGranted)
    ));
}

#[tokio::test]
async fn remint_rejects_obo_reentry_loop_guard() {
    // Present a token whose iss is the OBO issuer → loop guard, before any
    // signature check.
    let header = b64(&json!({ "alg": "ES256", "typ": "obo+jwt", "kid": "obo-token-sign-v1" }));
    let payload = b64(&json!({ "iss": OBO_ISS }));
    let obo_like = format!("{header}.{payload}.{}", URL_SAFE_NO_PAD.encode([0u8; 64]));
    let (_cap, jwks) = sign_cap_jwt(CAP_ISS, real_now() + 300);
    let svc = obo_service(Some(granted_adapter(&["quotas:read"])), jwks).await;
    assert!(matches!(
        svc.remint_obo(&peer(), &obo_like, None).await,
        Err(DomainError::LoopGuard)
    ));
}

#[tokio::test]
async fn remint_rejects_bad_cap_signature() {
    let (cap_jwt, jwks) = sign_cap_jwt(CAP_ISS, real_now() + 300);
    // Tamper the signature's last char.
    let mut parts: Vec<String> = cap_jwt.split('.').map(str::to_owned).collect();
    let sig = &parts[2];
    let last = sig.chars().next_back().unwrap();
    let repl = if last == 'A' { 'B' } else { 'A' };
    let mut chars: Vec<char> = sig.chars().collect();
    let n = chars.len();
    chars[n - 1] = repl;
    parts[2] = chars.into_iter().collect();
    let tampered = parts.join(".");

    let svc = obo_service(Some(granted_adapter(&["quotas:read"])), jwks).await;
    assert!(matches!(
        svc.remint_obo(&peer(), &tampered, None).await,
        Err(DomainError::CapInvalid { .. })
    ));
}

#[tokio::test]
async fn remint_rejects_expired_cap() {
    // exp well in the past, beyond leeway.
    let (cap_jwt, jwks) = sign_cap_jwt(CAP_ISS, real_now() - 600);
    let svc = obo_service(Some(granted_adapter(&["quotas:read"])), jwks).await;
    assert!(matches!(
        svc.remint_obo(&peer(), &cap_jwt, None).await,
        Err(DomainError::CapInvalid { .. })
    ));
}

#[tokio::test]
async fn remint_is_idempotent_by_cap_jti_and_scope() {
    let (cap_jwt, jwks) = sign_cap_jwt(CAP_ISS, real_now() + 300);
    let svc = obo_service(Some(granted_adapter(&["quotas:read"])), jwks).await;
    let a = svc.remint_obo(&peer(), &cap_jwt, None).await.unwrap();
    let b = svc.remint_obo(&peer(), &cap_jwt, None).await.unwrap();
    assert_eq!(a, b, "same cap + scope returns the cached OBO");
}

#[tokio::test]
async fn remint_idempotent_within_cap_skew_window() {
    // Regression (design review #11): the idempotency cache must stay live until
    // the cap's Gate-1 acceptance horizon (exp + clock_skew_secs), not bare exp.
    // A cap past exp but still within skew is accepted by Gate 1, so a retry
    // there MUST reuse the cached OBO rather than churn a fresh one.
    //
    // `verify_cap` uses the real wall clock (no injectable clock), so the cache
    // must run on the real clock too — leaving only the skew as margin. To stay
    // robust against scheduling we widen the configured skew: the *buggy* bare-exp
    // horizon (real_now - 5) is already in the past and evicts immediately, while
    // the *correct* exp + skew horizon stays ~1000 s ahead. The two outcomes are
    // unambiguous and not timing-fragile.
    let mut cfg = obo_config();
    cfg.cap_ttl_secs = 2_000;
    cfg.cap_reuse_floor_secs = 1_000;
    cfg.clock_skew_secs = 1_000;
    cfg.obo_ttl_secs = 1_000; // widen the OBO lifetime too, so neither horizon is timing-fragile
    let (cap_jwt, jwks) = sign_cap_jwt(CAP_ISS, real_now() - 5); // past exp, well within skew
    let peer_resolver: Arc<dyn PeerIdentityResolver> = Arc::new(MockPeerResolver {
        gts_id: ADAPTER_GTS.to_owned(),
    });
    let registry: Arc<dyn RmsAdapterRegistry> = Arc::new(MockRegistry {
        record: Some(granted_adapter(&["quotas:read"])),
    });
    let svc = Service::new(
        Arc::new(MockSigner { key_version: 3 }),
        peer_resolver,
        registry,
        &cfg,
        Arc::new(TokenIssuerMetrics::from_global()),
    )
    .unwrap()
    .with_clock(Arc::new(|| chrono::Utc::now().timestamp()));
    install_cap_jwks(&svc, jwks).await;

    let a = svc.remint_obo(&peer(), &cap_jwt, None).await.unwrap();
    let b = svc.remint_obo(&peer(), &cap_jwt, None).await.unwrap();
    assert_eq!(a, b, "cap within the skew window must reuse the cached OBO");
}

#[tokio::test]
async fn mint_capability_rejects_invalid_request() {
    let svc = service(1);

    // Empty audience.
    let mut req = sample_req();
    req.audience = "  ".to_owned();
    assert!(matches!(
        svc.mint_capability(&test_ctx(), req).await,
        Err(TokenIssuerError::InvalidRequest { .. })
    ));

    // Over-long audience.
    let mut req = sample_req();
    req.audience = "a".repeat(257);
    assert!(matches!(
        svc.mint_capability(&test_ctx(), req).await,
        Err(TokenIssuerError::InvalidRequest { .. })
    ));

    // Bad charset in operation.
    let mut req = sample_req();
    req.operation = Some("drop table;".to_owned());
    assert!(matches!(
        svc.mint_capability(&test_ctx(), req).await,
        Err(TokenIssuerError::InvalidRequest { .. })
    ));
}

#[tokio::test]
async fn mint_capability_refuses_under_obo_bearer() {
    let svc = service(2);
    let ctx = SecurityContext::builder()
        .subject_id(Uuid::from_u128(1))
        .subject_tenant_id(Uuid::from_u128(2))
        .subject_type("user")
        .token_scopes(vec!["a".to_owned()])
        // Inbound bearer minted by the OBO issuer → must be refused.
        .bearer_token(SecretString::from(format!(
            "{}.{}.{}",
            b64(&json!({ "alg": "ES256", "typ": "obo+jwt" })),
            b64(&json!({ "iss": OBO_ISS })),
            URL_SAFE_NO_PAD.encode([0u8; 64])
        )))
        .build()
        .expect("ctx");
    assert!(matches!(
        svc.mint_capability(&ctx, sample_req()).await,
        Err(TokenIssuerError::InvalidRequest { .. })
    ));
}

// ─── mint_grant (grant+jwt) ───────────────────────────────────────────────────

const GRANT_ISS: &str = "https://core.example.com/issuers/grant";

fn sample_grant_req() -> MintGrantRequest {
    MintGrantRequest {
        audience: ADAPTER_GTS.to_owned(),
        context_tenant: Uuid::from_u128(0xACE),
        project_id: None,
        resource_id: Uuid::from_u128(0xBEEF),
        resource_name: "prod-assets".to_owned(),
        resource_type: "storage.bucket".to_owned(),
        operations: vec!["signed-url-write".to_owned()],
        ttl_secs: 120,
    }
}

#[tokio::test]
async fn mint_grant_produces_es256_grant_jwt() {
    // Real-key signer so the publishability gate (kid must appear in the grant
    // JWKS) is satisfied.
    let svc = pubkey_service(Arc::new(PubKeySigner::new(2)));
    let out = svc
        .mint_grant(&test_ctx(), sample_grant_req())
        .await
        .unwrap();
    let parts: Vec<&str> = out.token.split('.').collect();
    assert_eq!(parts.len(), 3);

    let hdr: serde_json::Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
    assert_eq!(hdr["alg"], "ES256");
    assert_eq!(hdr["typ"], "grant+jwt");
    assert_eq!(hdr["kid"], "grant-token-sign-v2");

    let claims: serde_json::Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
    assert_eq!(claims["iss"], GRANT_ISS);
    assert_eq!(claims["aud"], ADAPTER_GTS);
    assert_eq!(claims["context_tenant"], Uuid::from_u128(0xACE).to_string());
    assert_eq!(claims["resource_id"], Uuid::from_u128(0xBEEF).to_string());
    assert_eq!(claims["resource_name"], "prod-assets");
    assert_eq!(claims["resource_type"], "storage.bucket");
    assert_eq!(claims["operations"], json!(["signed-url-write"]));
    // No nbf; project_id omitted when absent; exp = iat + ttl (clock = 1_000).
    assert!(claims.get("nbf").is_none());
    assert!(claims.get("project_id").is_none());
    assert_eq!(claims["iat"], 1_000);
    assert_eq!(claims["exp"], 1_120);
    assert_eq!(out.expires_at, 1_120);
}

#[tokio::test]
async fn mint_grant_includes_project_id_when_present() {
    // Mirror of the absent-omitted assertion above: when a project IS supplied it
    // must be serialized into the claim (the attribution hint the adapter passes
    // through). Locks the `Some(_)` arm of the `skip_serializing_if` on `project_id`.
    let svc = pubkey_service(Arc::new(PubKeySigner::new(2)));
    let mut req = sample_grant_req();
    let project = Uuid::from_u128(0x000F_F1CE);
    req.project_id = Some(project);
    let out = svc.mint_grant(&test_ctx(), req).await.unwrap();
    let parts: Vec<&str> = out.token.split('.').collect();
    let claims: serde_json::Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
    assert_eq!(claims["project_id"], project.to_string());
}

#[tokio::test]
async fn mint_grant_defaults_ttl_when_zero() {
    // A zero TTL falls back to the configured default (grant_ttl_secs = 300).
    let svc = pubkey_service(Arc::new(PubKeySigner::new(1)));
    let mut req = sample_grant_req();
    req.ttl_secs = 0;
    let out = svc.mint_grant(&test_ctx(), req).await.unwrap();
    assert_eq!(out.expires_at, 1_300);
}

#[tokio::test]
async fn mint_grant_rejects_empty_operations() {
    let svc = pubkey_service(Arc::new(PubKeySigner::new(1)));
    let mut req = sample_grant_req();
    req.operations.clear();
    assert!(matches!(
        svc.mint_grant(&test_ctx(), req).await,
        Err(TokenIssuerError::InvalidRequest { .. })
    ));
}

#[tokio::test]
async fn mint_grant_rejects_ttl_above_hard_limit() {
    let svc = pubkey_service(Arc::new(PubKeySigner::new(1)));
    let mut req = sample_grant_req();
    req.ttl_secs = MAX_TOKEN_TTL_SECS + 1;
    assert!(matches!(
        svc.mint_grant(&test_ctx(), req).await,
        Err(TokenIssuerError::InvalidRequest { .. })
    ));
}

#[tokio::test]
async fn grant_discovery_points_at_jwks() {
    let svc = service(1);
    let disc = svc.grant_discovery();
    assert_eq!(disc["issuer"], GRANT_ISS);
    assert_eq!(disc["jwks_uri"], format!("{GRANT_ISS}/jwks.json"));
}

#[tokio::test]
async fn grant_kid_is_isolated_from_the_cap_jwks() {
    // Cross-class isolation: a grant's kid (grant-token-sign-v*) is never present
    // in the capability JWKS, so a cap-only verifier cannot resolve a grant key.
    let svc = pubkey_service(Arc::new(PubKeySigner::new(1)));
    svc.warm_jwks().await.unwrap();
    let out = svc
        .mint_grant(&test_ctx(), sample_grant_req())
        .await
        .unwrap();
    let hdr: serde_json::Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(out.token.split('.').next().unwrap())
            .unwrap(),
    )
    .unwrap();
    let grant_kid = hdr["kid"].as_str().unwrap();
    let cap = svc.cap_jwks().await.unwrap();
    let cap_kids: Vec<&str> = cap["keys"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|k| k["kid"].as_str())
        .collect();
    assert!(
        !cap_kids.contains(&grant_kid),
        "grant kid {grant_kid} must not appear in cap JWKS {cap_kids:?}"
    );
    // And the grant JWKS carries the grant kid.
    let grant = svc.grant_jwks().await.unwrap();
    assert!(
        grant["keys"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|k| k["kid"].as_str())
            .any(|kid| kid == grant_kid)
    );
}

/// Signs a grant-shaped JWT (`typ = grant+jwt`, `iss = GRANT_ISS`) with a fresh
/// P-256 key — used to prove the OBO re-mint endpoint rejects a presented grant.
fn sign_grant_jwt(exp: i64) -> String {
    let key = SigningKey::random(&mut OsRng);
    let header = b64(&json!({ "alg": "ES256", "typ": "grant+jwt", "kid": "grant-token-sign-v1" }));
    let claims = json!({
        "iss": GRANT_ISS,
        "aud": ADAPTER_GTS,
        "sub": Uuid::from_u128(0x1111),
        "subject_tenant": Uuid::from_u128(0x2222),
        "context_tenant": Uuid::from_u128(0x42),
        "resource_id": Uuid::from_u128(0xBEEF),
        "resource_name": "prod-assets",
        "resource_type": "storage.bucket",
        "operations": ["signed-url-write"],
        "jti": Uuid::from_u128(0x9999),
        "iat": exp - 120,
        "exp": exp,
    });
    let signing_input = format!("{header}.{}", b64(&claims));
    let sig: Signature = key.sign(signing_input.as_bytes());
    format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(sig.to_bytes()))
}

#[tokio::test]
async fn obo_remint_rejects_grant_jwt_cross_class() {
    // A grant+jwt presented to the OBO re-mint endpoint is rejected at Gate 1
    // provenance: verify_cap requires typ == cap+jwt (and the grant issuer is not
    // in the OBO trust set), so it fails CapInvalid — never re-minted as OBO.
    let (_cap_jwt, jwks) = sign_cap_jwt(CAP_ISS, real_now() + 300);
    let svc = obo_service(Some(granted_adapter(&["quotas:read"])), jwks).await;
    let grant_jwt = sign_grant_jwt(real_now() + 300);
    assert!(matches!(
        svc.remint_obo(&peer(), &grant_jwt, None).await,
        Err(DomainError::CapInvalid { .. })
    ));
}
