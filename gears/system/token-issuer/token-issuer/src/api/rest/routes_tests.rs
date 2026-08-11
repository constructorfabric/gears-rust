#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use p256::ecdsa::SigningKey;
use p256::pkcs8::{EncodePublicKey, LineEnding};
use rand_core::OsRng;
use tower::ServiceExt as _;

use token_issuer_sdk::{
    PublicKeyVersion, SigAlg, SignatureResult, SigningClientV1, SigningError, SigningKeyRef,
};
use toolkit::api::OpenApiRegistryImpl;
use toolkit_security::SecurityContext;

use super::register_routes;
use crate::config::TokenIssuerConfig;
use crate::domain::error::DomainError;
use crate::domain::metrics::TokenIssuerMetrics;
use crate::domain::peer_identity::{PeerConnInfo, PeerIdentityResolver};
use crate::domain::rms_registry::{AdapterRecord, RmsAdapterRegistry};
use crate::domain::service::Service;

/// Signer that yields a real P-256 public key (so `warm_jwks` builds a valid
/// JWKS) and a fixed signature.
struct PubKeySigner {
    pem: String,
}

impl PubKeySigner {
    fn new() -> Self {
        let key = SigningKey::random(&mut OsRng);
        let pem = p256::PublicKey::from(key.verifying_key())
            .to_public_key_pem(LineEnding::LF)
            .unwrap();
        Self { pem }
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
            signature: vec![0u8; 64],
            key_version: 1,
        })
    }

    async fn public_keys(
        &self,
        _ctx: &SecurityContext,
        _key: &SigningKeyRef,
    ) -> Result<Vec<PublicKeyVersion>, SigningError> {
        Ok(vec![PublicKeyVersion {
            version: 1,
            alg: SigAlg::Es256,
            public_key_pem: self.pem.clone(),
        }])
    }
}

/// Peer resolver that always fail-closes (no mTLS cert), as in the gated MVP.
struct NoPeer;

#[async_trait]
impl PeerIdentityResolver for NoPeer {
    async fn resolve(&self, _peer: &PeerConnInfo) -> Result<String, DomainError> {
        Err(DomainError::PeerUnverified)
    }
}

struct EmptyRegistry;

#[async_trait]
impl RmsAdapterRegistry for EmptyRegistry {
    async fn lookup(&self, _gts_id: &str) -> Result<Option<AdapterRecord>, DomainError> {
        Ok(None)
    }
    async fn gts_id_by_cert_subject(&self, _subject: &str) -> Result<Option<String>, DomainError> {
        Ok(None)
    }
}

fn config(obo_enabled: bool) -> TokenIssuerConfig {
    let mut cfg = TokenIssuerConfig {
        issuer_base_url: "https://core.example.com".to_owned(),
        ..Default::default()
    };
    cfg.obo.enabled = obo_enabled;
    cfg
}

async fn router(obo_enabled: bool) -> Router {
    let svc = Arc::new(
        Service::new(
            Arc::new(PubKeySigner::new()),
            Arc::new(NoPeer),
            Arc::new(EmptyRegistry),
            &config(obo_enabled),
            Arc::new(TokenIssuerMetrics::from_global()),
        )
        .unwrap(),
    );
    svc.warm_jwks().await.expect("warm jwks");
    let openapi = OpenApiRegistryImpl::new();
    register_routes(Router::new(), &openapi, svc)
}

#[tokio::test]
async fn obo_routes_absent_when_disabled() {
    let r = router(false).await;

    // OBO JWKS not registered.
    let resp = r
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/issuers/obo/jwks.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // Re-mint endpoint not registered.
    let resp = r
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/v1/issuers/obo/tokens")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn cap_jwks_always_served() {
    let r = router(false).await;
    let resp = r
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/issuers/cap/jwks.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn grant_jwks_and_discovery_always_served() {
    // The grant issuer surface is `.public()` and always registered (adapters fetch
    // grant keys offline), independent of the OBO toggle.
    let r = router(false).await;
    for uri in [
        "/issuers/grant/jwks.json",
        "/issuers/grant/.well-known/openid-configuration",
    ] {
        let resp = r
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{uri} must be served");
    }
}

#[tokio::test]
async fn obo_jwks_served_when_enabled() {
    let r = router(true).await;
    let resp = r
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/issuers/obo/jwks.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn remint_without_bearer_is_401() {
    let r = router(true).await;
    let resp = r
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/v1/issuers/obo/tokens")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // No Authorization header → cap provenance failure → 401.
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn remint_with_unverifiable_bearer_is_401_not_404_or_500() {
    let r = router(true).await;
    // A syntactically present (but unverifiable) bearer gets past header parsing;
    // it is not an OBO token (no loop guard), so the cap is verified. With no
    // installed cap key for its kid the cap fails provenance → 401. The peer
    // fail-close (403) requires the cap to verify first, which needs a
    // locally-signed cap matching the served JWKS — that path is covered by the
    // service-layer tests. Here we assert the gated endpoint authenticates the
    // bearer shape and does not 404/500.
    let resp = r
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/v1/issuers/obo/tokens")
                .header("authorization", "Bearer not-a-real-jwt")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // Bearer present but unverifiable → 401 (cap provenance), never 404/500.
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
