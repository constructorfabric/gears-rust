//! OBO loop guard: detect a bearer token that was itself minted by the OBO
//! issuer, so the re-mint (and cap mint) path can refuse it and avoid an
//! OBO-on-OBO chain. Decode-only — the `iss` claim is read from the payload
//! segment without any signature check (the guard only needs the claimed
//! issuer; provenance is enforced elsewhere by full verification).

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

/// Returns `true` when `bearer` is a JWT whose `iss` claim equals `obo_issuer`.
///
/// Decode-only: base64url-no-pad-decodes the payload segment and reads `iss`.
/// A missing bearer, a malformed token, or any other issuer yields `false`.
#[must_use]
pub fn is_obo_reentry(bearer: Option<&str>, obo_issuer: &str) -> bool {
    let Some(jwt) = bearer else { return false };
    let Some(payload) = jwt.split('.').nth(1) else {
        return false;
    };
    let Ok(bytes) = URL_SAFE_NO_PAD.decode(payload) else {
        return false;
    };
    serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|v| {
            v.get("iss")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .is_some_and(|iss| iss == obo_issuer)
}

#[cfg(test)]
#[path = "loopguard_tests.rs"]
mod tests;
