//! Shared compact-JWS assembly over the [`SigningClientV1`] port.
//!
//! Both the capability and OBO signing paths sign twice on a miss: once with a
//! provisional header to learn the Transit key version, then once more with the
//! final header carrying `kid = {key}-v{version}`. `kid` is inside the signed
//! header and must match the version Transit actually used, so it cannot be
//! guessed ahead of the first signature.
//!
//! If Transit rotates *between* the version-learning sign and the final sign,
//! the header `kid` would name an older version than the one that actually
//! signed, yielding an unverifiable token. The final sign is therefore retried
//! (bounded) until the signing version matches the version baked into the
//! header; if it never stabilizes the mint fails closed (retryable) rather than
//! returning a bad token.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Serialize;
use token_issuer_sdk::{
    SignatureResult, SigningClientV1, SigningError, SigningKeyRef, TokenIssuerError,
};
use toolkit_security::SecurityContext;

/// Max attempts to obtain a final signature whose key version matches the
/// `kid` header (only ever exceeded under repeated mid-mint key rotation).
const MAX_KID_STABILIZE_ATTEMPTS: u8 = 4;

/// base64url-no-pad encode.
pub(crate) fn b64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Assembles and signs a compact JWS for `claims` with the given `typ` header,
/// signing with `key` via the `signer` port.
///
/// The optional `on_sign` hook fires after each successful signature (the
/// capability path uses it to record signing metrics; pass a no-op otherwise).
/// The header is `{ "alg":"ES256", "typ":<typ>, "kid":"{key}-v{version}" }`.
///
/// # Errors
/// Returns [`TokenIssuerError`] if claim serialization or signing fails.
pub(crate) async fn assemble_and_sign<C, F>(
    signer: &dyn SigningClientV1,
    ctx: &SecurityContext,
    key: &SigningKeyRef,
    typ: &str,
    claims: &C,
    mut on_sign: F,
) -> Result<String, TokenIssuerError>
where
    C: Serialize,
    F: FnMut(Result<&SignatureResult, &TokenIssuerError>),
{
    let payload_b64 =
        b64url(&serde_json::to_vec(claims).map_err(|e| TokenIssuerError::Internal(e.to_string()))?);

    // Provisional sign to learn the current key version.
    let header = serde_json::json!({ "alg": "ES256", "typ": typ });
    let header_b64 = b64url(
        &serde_json::to_vec(&header).map_err(|e| TokenIssuerError::Internal(e.to_string()))?,
    );
    let signing_input = format!("{header_b64}.{payload_b64}");
    let mut sig = sign_once(signer, ctx, key, &signing_input, &mut on_sign).await?;

    // Re-sign with the kid header. Retry if Transit rotated mid-mint (final
    // version != the version named in the header) so the kid always matches the
    // signing version; fail closed if it never stabilizes.
    for _ in 0..MAX_KID_STABILIZE_ATTEMPTS {
        let version = sig.key_version;
        let header = serde_json::json!({
            "alg": "ES256",
            "typ": typ,
            "kid": format!("{}-v{}", key.as_str(), version),
        });
        let header_b64 = b64url(
            &serde_json::to_vec(&header).map_err(|e| TokenIssuerError::Internal(e.to_string()))?,
        );
        let signing_input = format!("{header_b64}.{payload_b64}");
        sig = sign_once(signer, ctx, key, &signing_input, &mut on_sign).await?;
        if sig.key_version == version {
            return Ok(format!("{signing_input}.{}", b64url(&sig.signature)));
        }
    }
    Err(TokenIssuerError::Signing(
        SigningError::service_unavailable(
            "signing key version did not stabilize during minting (repeated rotation)",
        ),
    ))
}

/// Signs once, invoking `on_sign` with the (borrowed) outcome.
async fn sign_once<F>(
    signer: &dyn SigningClientV1,
    ctx: &SecurityContext,
    key: &SigningKeyRef,
    signing_input: &str,
    on_sign: &mut F,
) -> Result<SignatureResult, TokenIssuerError>
where
    F: FnMut(Result<&SignatureResult, &TokenIssuerError>),
{
    match signer.sign(ctx, key, signing_input.as_bytes()).await {
        Ok(sig) => {
            on_sign(Ok(&sig));
            Ok(sig)
        }
        Err(e) => {
            let err: TokenIssuerError = e.into();
            on_sign(Err(&err));
            Err(err)
        }
    }
}

#[cfg(test)]
#[path = "jws_tests.rs"]
mod tests;
