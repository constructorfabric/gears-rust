//! JWT validator — local signature and claims verification.
//!
//! Verifies JWT tokens using cached JWKS. Runtime configuration controls the
//! enabled subset of the plugin-supported RS256 and ES256 algorithms.
//!
//! Validation order:
//! 1. Decode and parse the JWT header (extract `kid`, `alg`, and `typ`).
//! 2. Reject unsupported/disallowed algorithms.
//! 3. Verify `iss` against the `trusted_issuers` list and resolve the matched
//!    issuer's per-issuer overrides (audience, JOSE `typ`, clock-skew leeway).
//! 4. When the matched issuer pins a JOSE `typ`, require the header `typ` to
//!    match it (case-insensitive).
//! 5. Look up the signing key by `kid` in the JWKS cache.
//!    - On miss: force-refresh from Oidc.
//!    - If still missing after refresh: return `Unauthorized`.
//! 6. Verify the JWT signature and decode claims (leeway = per-issuer override
//!    when present, else global).
//! 7. Validate `exp` (rejected if expired).
//! 8. Validate optional `iat` (rejected if issued too far in the future).
//! 9. Validate `aud` against the per-issuer audience when set, else the global
//!    audience.

use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::jwk::{KeyAlgorithm, PublicKeyUse};
use jsonwebtoken::{Algorithm, DecodingKey, Header, Validation, decode_header};
use serde::{Deserialize, Serialize};
use toolkit_macros::domain_model;
use tracing::{debug, warn};
use url::Url;

use crate::config::{IssuerTrustConfig, JwtValidationConfig, MatcherCompiled, ResolvedIssuer};
use crate::domain::error::AuthNError;
use crate::domain::metrics::{
    AuthNMetrics, TOKEN_REJECTION_REASON_EXPIRED, TOKEN_REJECTION_REASON_INVALID_AUDIENCE,
    TOKEN_REJECTION_REASON_INVALID_IAT, TOKEN_REJECTION_REASON_INVALID_SIG,
    TOKEN_REJECTION_REASON_INVALID_TYP, TOKEN_REJECTION_REASON_MISSING_AUDIENCE,
    TOKEN_REJECTION_REASON_UNTRUSTED_ISSUER,
};
use crate::domain::ports::JwksProvider;

/// Decoded JWT claims — the fields we care about for validation and mapping.
#[domain_model]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JwtClaims {
    /// Subject identifier (must be a valid UUID).
    pub sub: String,
    /// Issuer.
    pub iss: String,
    /// Expiry (Unix timestamp).
    pub exp: u64,
    /// Issued-at timestamp (optional Unix timestamp).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iat: Option<u64>,
    /// Audience (optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aud: Option<serde_json::Value>,
    /// Authorized party / client ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub azp: Option<String>,
    /// Client ID (alternative to azp in some flows).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// CF tenant ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    /// CF user type.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_type: Option<String>,
    /// Scopes (space-separated).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Any non-standard claims, preserved verbatim so claim mapping can be
    /// configured to read them by name (e.g. cap tokens' `subject_tenant`,
    /// `scopes`). An empty map serializes to nothing.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// JWT validator backed by a domain JWKS provider port.
#[domain_model]
pub struct JwtValidator {
    // Debug is implemented manually below to avoid exposing provider internals.
    jwks_provider: Arc<dyn JwksProvider>,
    /// Injected metrics handle for recording validation durations and rejections.
    metrics: Arc<AuthNMetrics>,
}

impl std::fmt::Debug for JwtValidator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JwtValidator").finish_non_exhaustive()
    }
}

impl JwtValidator {
    /// Create a new `JwtValidator`.
    #[must_use]
    pub fn new(jwks_provider: Arc<dyn JwksProvider>, metrics: Arc<AuthNMetrics>) -> Self {
        Self {
            jwks_provider,
            metrics,
        }
    }

    /// Validate a JWT bearer token.
    ///
    /// Returns the decoded [`JwtClaims`] on success.
    ///
    /// # Errors
    ///
    /// Returns [`AuthNError`] for any validation failure:
    /// - [`AuthNError::SignatureInvalid`]: cannot decode/verify token
    /// - [`AuthNError::UnsupportedAlgorithm`]: `alg` not enabled by configuration
    /// - [`AuthNError::UntrustedIssuer`]: `iss` not in `trusted_issuers`
    /// - [`AuthNError::InvalidTokenType`]: JOSE `typ` does not match the issuer's pinned type
    /// - [`AuthNError::KidNotFound`]: key not in JWKS after force-refresh
    /// - [`AuthNError::SignatureInvalid`]: signature verification failed
    /// - [`AuthNError::TokenExpired`]: `exp` is in the past
    /// - [`AuthNError::IdpUnreachable`]: JWKS fetch failed
    pub async fn validate(
        &self,
        token: &str,
        config: &JwtValidationConfig,
        issuer_trust: &IssuerTrustConfig,
    ) -> Result<JwtClaims, AuthNError> {
        let started_at = Instant::now();
        let result = self.validate_inner(token, config, issuer_trust).await;
        self.metrics
            .record_jwt_validation_duration(started_at.elapsed());
        result
    }

    /// Core JWT validation flow; wrapped by `validate()` for duration recording.
    async fn validate_inner(
        &self,
        token: &str,
        config: &JwtValidationConfig,
        issuer_trust: &IssuerTrustConfig,
    ) -> Result<JwtClaims, AuthNError> {
        // Step 1: Decode JWT header (no verification yet — just parsing)
        let header = Self::decode_header(token)?;

        // Step 2: Reject unsupported algorithms
        let alg = Self::validate_algorithm(&header, config)?;

        // Step 3: Peek at the issuer claim without full verification.
        // We need it to determine which JWKS to fetch. The issuer is also
        // validated post-signature via `Validation::set_issuer` (defense-in-depth).
        let issuer = Self::peek_issuer(token)?;

        // Step 4: Check issuer is trusted (uses IssuerTrustConfig for exact or
        // regex matching) and resolve its per-issuer validation overrides.
        let resolved = self.validate_issuer(&issuer, issuer_trust)?;

        // Step 4b: Enforce the issuer's JOSE `typ` requirement, if pinned.
        self.validate_jose_typ(&header, &resolved)?;

        // Effective per-issuer overrides (fall back to global where unset).
        let leeway = resolved
            .clock_skew_leeway_secs
            .unwrap_or(config.clock_skew_leeway_secs);
        let expected_audience = if resolved.expected_audience.is_empty() {
            config.expected_audience.as_slice()
        } else {
            resolved.expected_audience.as_slice()
        };

        // Step 5: Get the kid (may be absent for single-key issuers)
        let kid = header.kid.clone();

        // Step 6: Resolve the signing key, with force-refresh on miss
        let decoding_key = self
            .resolve_decoding_key(&issuer, &resolved.discovery_base, kid.as_deref(), alg)
            .await?;

        // Step 7: Build validation rules and decode + verify the token
        let mut validation = Validation::new(alg);
        validation.validate_nbf = true;
        validation.leeway = leeway;

        // Issuer validation (defense-in-depth: re-verified post-signature)
        validation.set_issuer(&[issuer.as_str()]);

        // Audience matching is handled after signature verification so `*`
        // patterns can be matched with the compiled config regexes. Missing
        // `aud` presence is enforced here.
        validation.validate_aud = false;

        // Require an `aud` claim when globally mandated OR when this issuer pins
        // its own audience: a per-issuer `expected_audience` must fail closed on
        // a missing `aud` (it binds tokens to a specific audience, e.g. an
        // adapter GTS-id) rather than silently depend on the global flag.
        if config.require_audience || !resolved.expected_audience.is_empty() {
            validation.set_required_spec_claims(&["aud"]);
        }

        let token_data = jsonwebtoken::decode::<JwtClaims>(token, &decoding_key, &validation)
            .map_err(|e| match e.kind() {
                jsonwebtoken::errors::ErrorKind::ExpiredSignature => {
                    self.metrics
                        .increment_token_rejected(TOKEN_REJECTION_REASON_EXPIRED);
                    AuthNError::TokenExpired
                }
                jsonwebtoken::errors::ErrorKind::InvalidAudience => {
                    self.metrics
                        .increment_token_rejected(TOKEN_REJECTION_REASON_INVALID_AUDIENCE);
                    AuthNError::InvalidAudience
                }
                jsonwebtoken::errors::ErrorKind::InvalidIssuer => {
                    self.metrics
                        .increment_token_rejected(TOKEN_REJECTION_REASON_UNTRUSTED_ISSUER);
                    AuthNError::UntrustedIssuer
                }
                jsonwebtoken::errors::ErrorKind::MissingRequiredClaim(claim) if claim == "aud" => {
                    self.metrics
                        .increment_token_rejected(TOKEN_REJECTION_REASON_MISSING_AUDIENCE);
                    AuthNError::MissingClaim("aud".to_owned())
                }
                _ => {
                    self.metrics
                        .increment_token_rejected(TOKEN_REJECTION_REASON_INVALID_SIG);
                    AuthNError::SignatureInvalid
                }
            })?;

        let claims = token_data.claims;

        if let Some(iat) = claims.iat
            && iat > current_unix_timestamp().saturating_add(leeway)
        {
            self.metrics
                .increment_token_rejected(TOKEN_REJECTION_REASON_INVALID_IAT);

            return Err(AuthNError::SignatureInvalid);
        }

        if let Some(aud) = &claims.aud
            && !expected_audience.is_empty()
            && !audience_matches(expected_audience, aud)
        {
            self.metrics
                .increment_token_rejected(TOKEN_REJECTION_REASON_INVALID_AUDIENCE);

            return Err(AuthNError::InvalidAudience);
        }

        debug!(sub = %claims.sub, "JWT validated successfully");

        Ok(claims)
    }

    /// Decode the JWT header without verifying the signature.
    fn decode_header(token: &str) -> Result<Header, AuthNError> {
        decode_header(token).map_err(|_e| AuthNError::SignatureInvalid)
    }

    /// Validate that the algorithm is in the allowed set.
    fn validate_algorithm(
        header: &Header,
        config: &JwtValidationConfig,
    ) -> Result<Algorithm, AuthNError> {
        let alg = header.alg;
        if config.supported_algorithms.contains(&alg) {
            Ok(alg)
        } else {
            Err(AuthNError::UnsupportedAlgorithm)
        }
    }

    /// Peek at the `iss` claim without verifying the signature.
    ///
    /// This is needed to determine which JWKS to use. The issuer is also
    /// validated post-signature by `Validation::set_issuer` (defense-in-depth).
    fn peek_issuer(token: &str) -> Result<String, AuthNError> {
        // The payload is the second base64url segment
        let parts: Vec<&str> = token.splitn(3, '.').collect();
        if parts.len() < 2 {
            return Err(AuthNError::SignatureInvalid);
        }

        // Decode with padding-tolerant base64url
        let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(parts[1])
            .map_err(|_e| AuthNError::SignatureInvalid)?;

        let payload: serde_json::Value =
            serde_json::from_slice(&payload_bytes).map_err(|_e| AuthNError::SignatureInvalid)?;

        payload
            .get("iss")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .ok_or_else(|| AuthNError::MissingClaim("iss".to_owned()))
    }

    /// Validate the issuer against the trusted issuers configuration.
    ///
    /// Delegates to [`IssuerTrustConfig::resolve_issuer`] which handles both
    /// exact (literal string) and regex (auto-anchored) matching modes, and
    /// returns the matched issuer's per-issuer validation overrides.
    fn validate_issuer(
        &self,
        issuer: &str,
        issuer_trust: &IssuerTrustConfig,
    ) -> Result<ResolvedIssuer, AuthNError> {
        if let Some(resolved) = issuer_trust.resolve_issuer(issuer) {
            Ok(resolved)
        } else {
            self.metrics
                .increment_token_rejected(TOKEN_REJECTION_REASON_UNTRUSTED_ISSUER);
            Err(AuthNError::UntrustedIssuer)
        }
    }

    /// Enforce the matched issuer's JOSE `typ` requirement, if any.
    ///
    /// When the issuer pins a `jose_typ`, the token's `typ` header must match it
    /// case-insensitively. Issuers without an override do not inspect `typ`.
    fn validate_jose_typ(
        &self,
        header: &Header,
        resolved: &ResolvedIssuer,
    ) -> Result<(), AuthNError> {
        let Some(expected_typ) = resolved.jose_typ.as_deref() else {
            return Ok(());
        };

        // `expected_typ` is stored lowercased; compare the header case-insensitively.
        let matches = header
            .typ
            .as_deref()
            .is_some_and(|typ| typ.eq_ignore_ascii_case(expected_typ));

        if matches {
            Ok(())
        } else {
            self.metrics
                .increment_token_rejected(TOKEN_REJECTION_REASON_INVALID_TYP);
            Err(AuthNError::InvalidTokenType)
        }
    }

    /// Resolve the [`DecodingKey`] for the given issuer and optional `kid`.
    ///
    /// Tries the cached JWKS first, force-refreshes on miss or ambiguous
    /// missing-`kid` selection, and errors if no key can be resolved after the
    /// refresh.
    async fn resolve_decoding_key(
        &self,
        issuer: &str,
        discovery_base: &Url,
        kid: Option<&str>,
        alg: Algorithm,
    ) -> Result<DecodingKey, AuthNError> {
        let jwks = self.jwks_provider.get_jwks(issuer, discovery_base).await?;

        if let Some(key) = Self::find_key_in_jwks(&jwks, kid, alg) {
            return Ok(key);
        }

        // Key not found (or selection was ambiguous without kid) — refresh and try again.
        warn!(
            issuer,
            kid, "signing key not resolved in JWKS, force-refreshing"
        );
        let refreshed = match self
            .jwks_provider
            .force_refresh(issuer, discovery_base)
            .await
        {
            Ok(jwks) => jwks,
            Err(error) => {
                self.metrics.increment_jwks_refresh_failures();
                return Err(error);
            }
        };

        Self::find_key_in_jwks(&refreshed, kid, alg).ok_or(AuthNError::KidNotFound)
    }

    /// Find a decoding key in a JWKS matching the given `kid` and algorithm.
    ///
    /// Tokens without `kid` are only accepted when the JWKS has a single usable
    /// signing key. Multiple signing keys make key selection ambiguous.
    fn find_key_in_jwks(jwks: &JwkSet, kid: Option<&str>, alg: Algorithm) -> Option<DecodingKey> {
        let mut unique_decoding_key = None;

        for jwk in &jwks.keys {
            if let Some(expected_kid) = kid
                && jwk.common.key_id.as_deref() != Some(expected_kid)
            {
                continue;
            }

            if let Some(ref key_use) = jwk.common.public_key_use
                && !matches!(key_use, PublicKeyUse::Signature)
            {
                continue;
            }

            if let Some(ref jwk_alg) = jwk.common.key_algorithm
                && !key_algorithm_matches(*jwk_alg, alg)
            {
                continue;
            }

            let Ok(decoding_key) = DecodingKey::from_jwk(jwk) else {
                continue;
            };

            if kid.is_some() {
                return Some(decoding_key);
            }

            if unique_decoding_key.replace(decoding_key).is_some() {
                return None;
            }
        }

        unique_decoding_key
    }
}

/// Match a JWK `KeyAlgorithm` against a JWT header `Algorithm`.
///
/// Only the algorithms accepted by config parsing are matched.
/// Keep this in sync when extending `jwt.supported_algorithms`.
fn key_algorithm_matches(jwk_alg: KeyAlgorithm, header_alg: Algorithm) -> bool {
    matches!(
        (jwk_alg, header_alg),
        (KeyAlgorithm::RS256, Algorithm::RS256) | (KeyAlgorithm::ES256, Algorithm::ES256)
    )
}

#[must_use]
pub fn validate_audience(matchers: &[MatcherCompiled], audience: &str) -> bool {
    matchers.iter().any(|matcher| matcher.is_match(audience))
}

fn audience_matches(matchers: &[MatcherCompiled], audience: &serde_json::Value) -> bool {
    match audience {
        serde_json::Value::String(audience) => validate_audience(matchers, audience),
        serde_json::Value::Array(audiences) => audiences.iter().any(|audience| {
            audience
                .as_str()
                .is_some_and(|audience| validate_audience(matchers, audience))
        }),
        _ => false,
    }
}

fn current_unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
#[path = "validator_tests.rs"]
mod validator_tests;
