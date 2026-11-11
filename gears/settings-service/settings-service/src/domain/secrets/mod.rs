//! Secret values: the opaque handle a consumer holds and the machine-only
//! resolution behind it.
//!
//! A `secret`-trait value never leaves this service in plaintext except
//! through [`SecretResolver::resolve`], reached by the SDK reader. What the
//! reader hands out for such a value is a [`SecretHandle`] naming the setting
//! and the requested scope and nothing else, so a consumer cannot take the
//! store reference around the reader, and the value is resolved again when the
//! handle is used.

pub mod pending;
pub mod service;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use settings_service_sdk::SecretHandle;
use settings_service_sdk::canonical::plaintext_text;
use toolkit_macros::domain_model;

use crate::domain::error::DomainError;
use crate::field;

pub use pending::{PendingSecret, PendingSecretDraft, PendingSecretRepository};
pub use service::SecretResolver;

/// The plaintext a secret declaration's placeholder default is handed out as:
/// the empty text for a `null` default, and otherwise the text
/// [`plaintext_text`] gives it.
///
/// A `null` placeholder (a type that admits `null`) says that no credential is
/// configured, so a consumer must see an empty one, never the four characters
/// `null` a credential could carry. `resolve_secret` hands this out and the
/// expected value of a change that falls back to the default is hashed over
/// it, so the two cannot drift. A stored credential's text is never taken
/// through here.
#[must_use]
pub fn default_plaintext(default: &Value) -> SecretString {
    match default {
        Value::Null => SecretString::from(String::new()),
        other => plaintext_text(other),
    }
}

/// The handle format version, so a later encoding can coexist with this one.
const HANDLE_PREFIX: &str = "sh1.";

/// What a handle was issued for.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandleClaims {
    /// The setting key.
    pub key: String,
    /// The scope the consumer asked for, as it asked for it.
    pub scope: String,
}

/// Issue the handle for a `secret`-classified setting at `scope`.
///
/// Nothing but the key and the scope ride in it: no store reference, no
/// winning tenant, no credential coordinates.
#[must_use]
pub fn issue_handle(key: &str, scope: &str) -> SecretHandle {
    // @cpt-begin:cpt-cf-settings-service-algo-secret-values-handle:p1:inst-sv-handle-1
    let claims = HandleClaims {
        key: key.to_owned(),
        scope: scope.to_owned(),
    };
    let json = serde_json::to_vec(&claims).unwrap_or_default();
    SecretHandle::new(format!("{HANDLE_PREFIX}{}", URL_SAFE_NO_PAD.encode(json)))
    // @cpt-end:cpt-cf-settings-service-algo-secret-values-handle:p1:inst-sv-handle-1
}

fn malformed() -> DomainError {
    DomainError::Validation {
        field: "handle".to_owned(),
        code: field::SECRET_HANDLE_MALFORMED,
        message: "the secret handle is malformed".to_owned(),
    }
}

/// Read a handle back into what it was issued for.
///
/// # Errors
/// [`DomainError::Validation`] when the token does not decode; the token is
/// not echoed.
pub fn decode_handle(handle: &SecretHandle) -> Result<HandleClaims, DomainError> {
    // @cpt-begin:cpt-cf-settings-service-algo-secret-values-handle:p1:inst-sv-handle-2
    let encoded = handle
        .as_token()
        .strip_prefix(HANDLE_PREFIX)
        .ok_or_else(malformed)?;
    let json = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| malformed())?;
    let claims: HandleClaims = serde_json::from_slice(&json).map_err(|_| malformed())?;
    if claims.key.is_empty() {
        return Err(malformed());
    }
    Ok(claims)
    // @cpt-end:cpt-cf-settings-service-algo-secret-values-handle:p1:inst-sv-handle-2
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "handle_tests.rs"]
mod handle_tests;
