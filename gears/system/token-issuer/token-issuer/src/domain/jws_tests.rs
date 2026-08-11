use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use token_issuer_sdk::{PublicKeyVersion, SignatureResult, SigningError};

use super::*;

/// Signer that returns a scripted sequence of `key_version`s across successive
/// `sign` calls (the last entry repeats), to drive the kid-stabilization loop.
struct ScriptedSigner {
    versions: Vec<u32>,
    idx: AtomicUsize,
}

impl ScriptedSigner {
    fn new(versions: Vec<u32>) -> Self {
        Self {
            versions,
            idx: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl SigningClientV1 for ScriptedSigner {
    async fn sign(
        &self,
        _ctx: &SecurityContext,
        _key: &SigningKeyRef,
        _signing_input: &[u8],
    ) -> Result<SignatureResult, SigningError> {
        let i = self.idx.fetch_add(1, Ordering::SeqCst);
        let key_version = self.versions[i.min(self.versions.len() - 1)];
        Ok(SignatureResult {
            signature: vec![0xAB; 64],
            key_version,
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

/// Extracts the `kid` from a compact JWS header.
fn kid_of(jwt: &str) -> String {
    let header_b64 = jwt.split('.').next().expect("header segment");
    let bytes = URL_SAFE_NO_PAD.decode(header_b64).expect("decode header");
    let header: serde_json::Value = serde_json::from_slice(&bytes).expect("parse header");
    header["kid"].as_str().expect("kid present").to_owned()
}

#[tokio::test]
async fn kid_matches_stable_signing_version() {
    let signer = ScriptedSigner::new(vec![5]);
    let ctx = SecurityContext::anonymous();
    let key = SigningKeyRef::new("cap-token-sign").unwrap();
    let jwt = assemble_and_sign(
        &signer,
        &ctx,
        &key,
        "cap+jwt",
        &serde_json::json!({"a": 1}),
        |_| {},
    )
    .await
    .expect("sign");
    assert_eq!(kid_of(&jwt), "cap-token-sign-v5");
}

#[tokio::test]
async fn re_signs_when_version_rotates_once() {
    // Provisional sign -> v1, every subsequent sign -> v2: the loop re-signs
    // with kid=v2 and stabilizes.
    let signer = ScriptedSigner::new(vec![1, 2]);
    let ctx = SecurityContext::anonymous();
    let key = SigningKeyRef::new("cap-token-sign").unwrap();
    let jwt = assemble_and_sign(
        &signer,
        &ctx,
        &key,
        "cap+jwt",
        &serde_json::json!({"a": 1}),
        |_| {},
    )
    .await
    .expect("sign");
    assert_eq!(kid_of(&jwt), "cap-token-sign-v2");
}

#[tokio::test]
async fn fails_closed_when_version_never_stabilizes() {
    // Every sign yields a fresh version: the kid can never match the signature.
    let signer = ScriptedSigner::new(vec![1, 2, 3, 4, 5, 6, 7, 8]);
    let ctx = SecurityContext::anonymous();
    let key = SigningKeyRef::new("cap-token-sign").unwrap();
    let err = assemble_and_sign(
        &signer,
        &ctx,
        &key,
        "cap+jwt",
        &serde_json::json!({"a": 1}),
        |_| {},
    )
    .await
    .expect_err("must fail closed under repeated rotation");
    assert!(matches!(err, TokenIssuerError::Signing(_)));
}
